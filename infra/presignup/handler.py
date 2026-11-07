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
    else:
        firm = _firm_of(provider, ssm)
        if firm is None or not _UUID.match(firm):
            raise _deny()

    pool = event["userPoolId"]
    found = client.list_users(UserPoolId=pool, Filter=f'email = "{email}"', Limit=10).get("Users", [])
    invited = [user for user in found if user.get("UserStatus") != "EXTERNAL_PROVIDER"]
    if len(invited) != 1:
        raise _deny()
    user = invited[0]
    tenant = _attribute(user, "custom:tenant_id")
    if not tenant or not _UUID.match(tenant.lower()):
        raise _deny()
    if firm is not None and tenant.lower() != str(firm).lower():
        raise _deny()

    client.admin_link_provider_for_user(
        UserPoolId=pool,
        DestinationUser={"ProviderName": "Cognito", "ProviderAttributeValue": user["Username"]},
        SourceUser={"ProviderName": provider, "ProviderAttributeName": "Cognito_Subject", "ProviderAttributeValue": subject},
    )
    # The invitation already confirmed the address; do not leave a second, unconfirmed account.
    event["response"]["autoConfirmUser"] = True
    event["response"]["autoVerifyEmail"] = True
    return event
