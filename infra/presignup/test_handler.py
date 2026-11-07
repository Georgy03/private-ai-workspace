import unittest

import handler

FIRM_A = "11111111-1111-4111-8111-111111111111"
FIRM_B = "22222222-2222-4222-8222-222222222222"
POOL = "us-east-2_Pool"


class FakeSsm:
    """The parameter store: provider name -> firm ID, under the path the function reads."""

    class exceptions:
        class ParameterNotFound(Exception):
            pass

    def __init__(self, firms):
        self.params = {handler.PARAMETER_PATH + name: firm for name, firm in firms.items()}
        self.asked = []

    def get_parameter(self, Name):
        self.asked.append(Name)
        if Name not in self.params:
            raise self.exceptions.ParameterNotFound()
        return {"Parameter": {"Name": Name, "Value": self.params[Name]}}


class FakeCognito:
    def __init__(self, users):
        self.users, self.filters, self.links = users, [], []

    def list_users(self, UserPoolId, Filter, Limit):
        self.filters.append(Filter)
        return {"Users": self.users}

    def admin_link_provider_for_user(self, **arguments):
        self.links.append(arguments)


def invited(tenant=FIRM_A, status="FORCE_CHANGE_PASSWORD", username="native-uuid"):
    attributes = [{"Name": "email", "Value": "sam@firm.example"}]
    if tenant:
        attributes.append({"Name": "custom:tenant_id", "Value": tenant})
    return {"Username": username, "UserStatus": status, "Attributes": attributes}


def event(provider="Google", email="Sam@Firm.example", verified="true", source="PreSignUp_ExternalProvider", subject="1234_5"):
    return {
        "triggerSource": source, "userPoolId": POOL, "userName": f"{provider}_{subject}",
        "request": {"userAttributes": {"email": email, "email_verified": verified}}, "response": {},
    }


class PreSignUp(unittest.TestCase):
    def setUp(self):
        self.ssm = FakeSsm({"ms-acme": FIRM_A})

    def refused(self, outgoing, cognito):
        with self.assertRaises(handler.NotInvited) as raised:
            handler.handler(outgoing, None, cognito, self.ssm)
        self.assertEqual(cognito.links, [], "nothing may be linked for someone who is refused")
        return str(raised.exception)

    def test_an_invited_person_signing_in_with_google_is_linked_to_their_invitation(self):
        cognito = FakeCognito([invited()])
        result = handler.handler(event(), None, cognito, self.ssm)
        self.assertEqual(cognito.filters, ['email = "sam@firm.example"'])
        self.assertEqual(cognito.links, [{
            "UserPoolId": POOL,
            "DestinationUser": {"ProviderName": "Cognito", "ProviderAttributeValue": "native-uuid"},
            "SourceUser": {"ProviderName": "Google", "ProviderAttributeName": "Cognito_Subject", "ProviderAttributeValue": "1234_5"},
        }])
        self.assertTrue(result["response"]["autoConfirmUser"] and result["response"]["autoVerifyEmail"])

    def test_an_email_google_has_not_verified_is_refused_without_looking_anyone_up(self):
        cognito = FakeCognito([invited()])
        for verified in ["false", "", "True1"]:
            self.refused(event(verified=verified), cognito)
        self.assertEqual(cognito.filters, [])

    def test_someone_who_was_not_invited_is_refused_and_told_to_ask_their_administrator(self):
        cognito = FakeCognito([])
        self.assertIn("not been invited", self.refused(event(), cognito))

    def test_a_person_already_signed_in_through_a_provider_is_not_an_invitation(self):
        cognito = FakeCognito([invited(status="EXTERNAL_PROVIDER")])
        self.refused(event(), cognito)

    def test_two_accounts_with_one_address_are_refused_rather_than_guessed(self):
        cognito = FakeCognito([invited(username="a"), invited(username="b")])
        self.refused(event(), cognito)

    def test_an_invitation_with_no_firm_is_refused(self):
        for tenant in [None, "not-a-uuid", ""]:
            self.refused(event(), FakeCognito([invited(tenant=tenant)]))

    def test_microsoft_works_only_for_a_person_invited_to_the_firm_that_provider_belongs_to(self):
        cognito = FakeCognito([invited(tenant=FIRM_A)])
        handler.handler(event(provider="ms-acme", verified="false"), None, cognito, self.ssm)
        self.assertEqual(cognito.links[0]["SourceUser"]["ProviderName"], "ms-acme")
        # The same address invited to a different firm: refused, whatever the email says.
        self.refused(event(provider="ms-acme"), FakeCognito([invited(tenant=FIRM_B)]))

    def test_a_provider_that_is_not_a_known_firms_is_refused(self):
        for provider in ["ms-unknown", "Facebook", "COGNITO", ""]:
            self.refused(event(provider=provider), FakeCognito([invited()]))
        self.ssm.params[handler.PARAMETER_PATH + "ms-acme"] = "not-a-uuid"
        self.refused(event(provider="ms-acme"), FakeCognito([invited()]))

    def test_removing_a_firms_record_stops_its_sign_ins_at_once(self):
        handler.handler(event(provider="ms-acme"), None, FakeCognito([invited()]), self.ssm)
        del self.ssm.params[handler.PARAMETER_PATH + "ms-acme"]
        self.refused(event(provider="ms-acme"), FakeCognito([invited()]))

    def test_a_provider_name_that_could_point_somewhere_else_is_never_looked_up(self):
        for provider in ["ms-../other/x", "ms-", "ms-UPPER", "ms-" + "a" * 40, "ms-a/b", "ms-a b"]:
            self.refused(event(provider=provider, subject="1"), FakeCognito([invited()]))
        self.assertEqual(self.ssm.asked, [], "a name that does not look like ours must not reach the parameter store")

    def test_a_firm_id_written_in_capitals_still_matches_the_invitation(self):
        self.ssm.params[handler.PARAMETER_PATH + "ms-acme"] = " " + FIRM_A.upper() + "\n"
        handler.handler(event(provider="ms-acme"), None, FakeCognito([invited(tenant=FIRM_A)]), self.ssm)

    def test_an_address_that_could_alter_the_search_is_refused_before_cognito_is_asked(self):
        cognito = FakeCognito([invited()])
        for email in ['x" or email ^= "', 'a\\b@firm.example', "no-at-sign", "", "a@b", "sam@firm.example\nx@y.zz"]:
            self.refused(event(email=email), cognito)
        self.assertEqual(cognito.filters, [])

    def test_accounts_made_by_an_administrator_or_by_sign_up_are_left_alone(self):
        cognito = FakeCognito([])
        for source in ["PreSignUp_AdminCreateUser", "PreSignUp_SignUp"]:
            outgoing = event(source=source)
            self.assertIs(handler.handler(outgoing, None, cognito, self.ssm), outgoing)
        self.assertEqual((cognito.filters, cognito.links), ([], []))


if __name__ == "__main__":
    unittest.main()
