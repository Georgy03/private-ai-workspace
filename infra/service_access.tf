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
