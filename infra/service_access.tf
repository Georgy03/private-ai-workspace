# The API and the worker run on Railway, outside AWS, so they cannot use an ECS task role. They share one
# IAM user that may do only one thing: use the firm keys for connector tokens (`token_keys` in iam.tf).
# Its access key is made by hand (see README.md), not here, so the secret never enters the state file.
#
# Making a firm's key is a rare, deliberate step done by an administrator with their own AWS sign-in
# (`workspace-cloud provision-firm`); no standing credential for it is created here.
resource "aws_iam_user" "services" {
  name = "${local.prefix}-services"
}

resource "aws_iam_user_policy" "services_token_keys" {
  name   = "connector-token-keys"
  user   = aws_iam_user.services.name
  policy = data.aws_iam_policy_document.token_keys.json
}

# The sign-in screen asks the API, before anyone is signed in, which Microsoft sign-in an email belongs to
# (`POST /v1/sign-in/route`). To answer, the API reads that one person's invitation and the record of which
# firm each provider belongs to. It cannot create or change anything.
data "aws_iam_policy_document" "services_sign_in_lookup" {
  statement {
    sid       = "ReadOneInvitation"
    actions   = ["cognito-idp:AdminGetUser"]
    resources = [aws_cognito_user_pool.main.arn]
  }

  statement {
    sid     = "ReadWhichFirmEachProviderBelongsTo"
    actions = ["ssm:GetParametersByPath"]
    resources = [
      "arn:${local.partition}:ssm:${var.region}:${local.account_id}:parameter${trimsuffix(local.firm_parameter_path, "/")}",
      "arn:${local.partition}:ssm:${var.region}:${local.account_id}:parameter${local.firm_parameter_path}*",
    ]
  }
}

resource "aws_iam_user_policy" "services_sign_in_lookup" {
  name   = "sign-in-lookup"
  user   = aws_iam_user.services.name
  policy = data.aws_iam_policy_document.services_sign_in_lookup.json
}

# Open sign-up: a person with no firm may start their own from the app. This is the one place the API gets the
# power to make things, so it is a switch (`self_serve_firms`), off until you want it, and as narrow as AWS
# allows: only keys tagged for this app, only the key's own alias, and no right to delete or use any key.
# It CAN attach a person to a firm in the user pool, which is the cost of doing this inside the API: if the API
# were taken over, it could be used to put someone into a firm. Moving this into a small service of its own
# with its own access key removes that, and is the next step once there are paying customers.
data "aws_iam_policy_document" "services_start_firm" {
  statement {
    sid       = "AttachANewPersonToTheirFirm"
    actions   = ["cognito-idp:AdminUpdateUserAttributes", "cognito-idp:AdminAddUserToGroup"]
    resources = [aws_cognito_user_pool.main.arn]
  }

  statement {
    sid       = "CreateOnlyOurKeys"
    actions   = ["kms:CreateKey", "kms:TagResource"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:RequestTag/app"
      values   = [local.app_tag]
    }
    condition {
      test     = "Null"
      variable = "aws:RequestTag/tenant"
      values   = ["false"]
    }
    condition {
      test     = "ForAllValues:StringEquals"
      variable = "aws:TagKeys"
      values   = ["app", "tenant", "firm"]
    }
  }

  statement {
    sid       = "SetUpANewFirmKey"
    actions   = ["kms:DescribeKey", "kms:EnableKeyRotation", "kms:CreateAlias"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/app"
      values   = [local.app_tag]
    }
  }

  statement {
    sid       = "NameANewFirmKey"
    actions   = ["kms:CreateAlias"]
    resources = ["arn:${local.partition}:kms:${var.region}:${local.account_id}:alias/prism-tenant-*"]
  }
}

resource "aws_iam_user_policy" "services_start_firm" {
  count  = var.self_serve_firms ? 1 : 0
  name   = "start-a-firm"
  user   = aws_iam_user.services.name
  policy = data.aws_iam_policy_document.services_start_firm.json
}
