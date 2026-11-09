"""Cognito "pre sign-up" trigger: only people who were invited can sign in with Google or Microsoft.

Cognito creates an account the first time someone signs in through an outside provider. Without this
check, anyone with a Google or Microsoft account could become a user of the pool. Here, an
administrator first creates each person (their email, verified, and the firm they belong to in
custom:tenant_id). On their first outside sign-in this function finds that invitation and links the
outside identity to it, so the tokens carry the invited account's firm and groups. Anyone not invited
is refused.

Two rules keep one firm out of another:
- Google is one provider for everyone, and only an email Google itself has verified is accepted.
- Each firm's Microsoft sign-in is its own provider, tied to that firm's Microsoft tenant, and only
  people invited to that same firm may use it. Microsoft's email claim is not proof of identity across
  tenants, so the provider, not the email, decides the firm.

Which firm a Microsoft provider belongs to is read from the AWS parameter store, one parameter per provider
(`/prism/signin/microsoft/<provider>` holds the firm's ID), written by `workspace-cloud provision-firm` and
`add-microsoft`. A provider with no parameter is refused, so removing the parameter stops its sign-ins at once.
"""

import os
import re

# No quote or backslash, because the address goes into a Cognito filter expression.
_EMAIL = re.compile(r'^[^@\s"\\]{1,64}@[^@\s"\\]{1,255}\.[^@\s"\\]{2,63}$')
_UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
# The names `workspace-cloud` gives Microsoft providers: "ms-", then letters, digits and hyphens.
_PROVIDER = re.compile(r"^ms-[a-z0-9-]{1,29}$")
# One provider serves every personal Microsoft account (workspace-cloud `add-microsoft-personal`).
PERSONAL_PROVIDER = "MicrosoftPersonal"
# Microsoft does not say whether a personal account's email is verified. For the addresses Microsoft itself issues,
# the address IS the account, so only those are accepted; any other address used to sign in to a personal Microsoft
# account (say, a company address) is refused here and should use Google or its firm's own sign-in.
PERSONAL_DOMAINS = frozenset({
    "outlook.com", "hotmail.com", "live.com", "msn.com", "outlook.co.uk", "hotmail.co.uk", "live.co.uk",
    "outlook.ca", "hotmail.ca", "live.ca", "outlook.com.au", "hotmail.com.au", "live.com.au",
    "outlook.de", "hotmail.de", "live.de", "outlook.fr", "hotmail.fr", "live.fr",
    "outlook.it", "hotmail.it", "live.it", "outlook.es", "hotmail.es", "outlook.in", "outlook.ie", "live.ie",
})
PARAMETER_PATH = os.environ.get("FIRM_PARAMETER_PATH", "/prism/signin/microsoft/")


class NotInvited(Exception):
    """Shown to the person by Cognito as the reason sign-in failed."""


def _deny():
    return NotInvited("This account has not been invited to a firm. Ask your administrator.")


def _attribute(user, name):
    for attribute in user.get("Attributes", []):
        if attribute["Name"] == name:
            return attribute["Value"]
    return None


def _firm_of(provider, ssm):
    """The firm a Microsoft provider belongs to, or None if it is not one we made."""
    if not _PROVIDER.match(provider):
        return None
    try:
        return ssm.get_parameter(Name=PARAMETER_PATH + provider)["Parameter"]["Value"].strip().lower()
    except ssm.exceptions.ParameterNotFound:
        return None


def handler(event, context=None, client=None, ssm=None):
    # Accounts made by an administrator, and sign-ups, are not outside sign-ins.
    if event.get("triggerSource") != "PreSignUp_ExternalProvider":
        return event
    if client is None:
        import boto3  # only inside Lambda, where it is provided

        client = boto3.client("cognito-idp")
    if ssm is None:
        import boto3

        ssm = boto3.client("ssm")

    provider, _, subject = str(event.get("userName", "")).partition("_")
    attributes = event.get("request", {}).get("userAttributes", {})
    email = str(attributes.get("email") or "").strip().lower()
    if not subject or not _EMAIL.match(email):
        raise _deny()

    firm = None
    if provider == "Google":
        if str(attributes.get("email_verified", "")).lower() != "true":
            raise _deny()
    elif provider == PERSONAL_PROVIDER:
        if email.rsplit("@", 1)[-1] not in PERSONAL_DOMAINS:
            raise _deny()
    else:
        firm = _firm_of(provider, ssm)
        if firm is None or not _UUID.match(firm):
            raise _deny()

    pool = event["userPoolId"]
    found = client.list_users(UserPoolId=pool, Filter=f'email = "{email}"', Limit=10).get("Users", [])
    if not found and (provider == "Google" or provider == PERSONAL_PROVIDER):
        # Nobody has this address, and Google has verified it: a newcomer who will start their own firm. They get
        # an account with no firm, which can do exactly one thing: ask the server to make them one (and become its
        # administrator). A firm's own Microsoft providers are not offered this, because they belong to firms.
        event["response"]["autoConfirmUser"] = True
        event["response"]["autoVerifyEmail"] = True
        return event
    invited = [user for user in found if user.get("UserStatus") != "EXTERNAL_PROVIDER"]
    if len(invited) != 1:
        raise _deny()
    user = invited[0]
    tenant = _attribute(user, "custom:tenant_id")
    if not tenant or not _UUID.match(tenant.lower()):
        raise _deny()
    if firm is not None and tenant.lower() != str(firm).lower():
        raise _deny()

    try:
        client.admin_link_provider_for_user(
            UserPoolId=pool,
            DestinationUser={"ProviderName": "Cognito", "ProviderAttributeValue": user["Username"]},
            SourceUser={"ProviderName": provider, "ProviderAttributeName": "Cognito_Subject", "ProviderAttributeValue": subject},
        )
    except Exception as error:  # noqa: BLE001 - only the one harmless case is let through, below
        # Cognito runs this function again on later sign-ins by a person who is already linked. That is
        # not a failure: the person is invited and linked, so let them through. Anything else still fails.
        if "already linked" not in str(error):
            raise
    # The invitation already confirmed the address; do not leave a second, unconfirmed account.
    event["response"]["autoConfirmUser"] = True
    event["response"]["autoVerifyEmail"] = True
    return event
