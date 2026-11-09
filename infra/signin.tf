# Sign in with Google or Microsoft, for invited people only.
#
# An administrator first creates each person in the pool (their verified email, and the firm they belong
# to in custom:tenant_id). When that person signs in with Google or Microsoft for the first time, the
# pre sign-up function (presignup/handler.py) links the outside identity to the invitation, so the
# tokens carry the invited account's firm and groups. Anyone not invited is refused, which keeps the
# pool from filling with accounts that have no firm.
#
# Google is one provider for all firms, made here. Microsoft cannot be: Cognito checks that a token's issuer is
# the provider's issuer exactly, and a Microsoft work account's issuer belongs to that firm's own Microsoft
# tenant, so the shared "common" address fails. Each firm therefore gets its own Microsoft provider, tied to
# its tenant and to its firm ID, and only people invited to that firm can use it.
#
# Those providers are NOT made here: `workspace-cloud provision-firm --microsoft-tenant <id>` (or
# `add-microsoft`) makes the provider, records which firm it belongs to in the parameter store, and allows it on
# the app, so adding a firm never changes this code. The function below reads that record.

locals {
  google_enabled = var.google_client_id != ""
  # Where each Microsoft provider's firm is recorded (backend/src/signin.rs, PARAMETER_PATH).
  firm_parameter_path = "/prism/signin/microsoft/"
}

# ---- the pre sign-up function ----

data "archive_file" "presignup" {
  type        = "zip"
  source_file = "${path.module}/presignup/handler.py"
  output_path = "${path.module}/.build/presignup.zip"
}

data "aws_iam_policy_document" "lambda_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "presignup" {
  name               = "${local.prefix}-presignup"
  assume_role_policy = data.aws_iam_policy_document.lambda_assume.json
}

resource "aws_cloudwatch_log_group" "presignup" {
  name              = "/aws/lambda/${local.prefix}-presignup"
  retention_in_days = 30
}

# It may write its own log and look up and link users in this pool, and nothing else.
data "aws_iam_policy_document" "presignup" {
  statement {
    sid       = "OwnLog"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.presignup.arn}:*"]
  }

  statement {
    sid       = "ReadWhichFirmAProviderBelongsTo"
    actions   = ["ssm:GetParameter"]
    resources = ["arn:${local.partition}:ssm:${var.region}:${local.account_id}:parameter${local.firm_parameter_path}*"]
  }

  statement {
    sid       = "FindAndLinkInvitedPeople"
    actions   = ["cognito-idp:ListUsers", "cognito-idp:AdminLinkProviderForUser"]
    resources = [aws_cognito_user_pool.main.arn]
  }
}

resource "aws_iam_role_policy" "presignup" {
  name   = "find-and-link-invited-people"
  role   = aws_iam_role.presignup.id
  policy = data.aws_iam_policy_document.presignup.json
}

resource "aws_lambda_function" "presignup" {
  function_name    = "${local.prefix}-presignup"
  role             = aws_iam_role.presignup.arn
  runtime          = "python3.12"
  handler          = "handler.handler"
  filename         = data.archive_file.presignup.output_path
  source_code_hash = data.archive_file.presignup.output_base64sha256
  timeout          = 10
  memory_size      = 128

  environment {
    variables = {
      FIRM_PARAMETER_PATH = local.firm_parameter_path
    }
  }

  depends_on = [aws_cloudwatch_log_group.presignup]
}

resource "aws_lambda_permission" "presignup" {
  statement_id  = "AllowCognito"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.presignup.function_name
  principal     = "cognito-idp.amazonaws.com"
  source_arn    = aws_cognito_user_pool.main.arn
}

# ---- the providers ----

resource "aws_cognito_identity_provider" "google" {
  count         = local.google_enabled ? 1 : 0
  user_pool_id  = aws_cognito_user_pool.main.id
  provider_name = "Google"
  provider_type = "Google"

  provider_details = {
    client_id        = var.google_client_id
    client_secret    = var.google_client_secret
    authorize_scopes = "openid email profile"
  }

  # Cognito only copies what is mapped. Without email_verified the pre sign-up function never sees Google's
  # confirmation of the address and (correctly) refuses everyone.
  attribute_mapping = {
    email          = "email"
    email_verified = "email_verified"
    username       = "sub"
  }
}
