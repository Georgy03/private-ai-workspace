# Sign in with Google or Microsoft, for invited people only.
#
# An administrator first creates each person in the pool (their verified email, and the firm they belong
# to in custom:tenant_id). When that person signs in with Google or Microsoft for the first time, the
# pre sign-up function (presignup/handler.py) links the outside identity to the invitation, so the
# tokens carry the invited account's firm and groups. Anyone not invited is refused, which keeps the
# pool from filling with accounts that have no firm.
#
# Google is one provider for all firms. Microsoft cannot be: Cognito checks that a token's issuer is the
# provider's issuer exactly, and a Microsoft work account's issuer belongs to that firm's own Microsoft
# tenant, so the shared "common" address fails. Each firm therefore gets its own Microsoft provider,
# tied to its tenant and to its firm ID, and only people invited to that firm can use it.

locals {
  google_enabled    = var.google_client_id != ""
  microsoft_enabled = var.microsoft_client_id != ""
  microsoft_firms   = local.microsoft_enabled ? var.microsoft_firms : {}
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
      # Which firm each Microsoft provider belongs to. The function refuses anyone invited to another firm.
      FIRM_BY_PROVIDER = jsonencode({ for name, firm in local.microsoft_firms : "ms-${name}" => firm.firm_id })
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

  attribute_mapping = {
    email    = "email"
    username = "sub"
  }
}

resource "aws_cognito_identity_provider" "microsoft" {
  for_each      = local.microsoft_firms
  user_pool_id  = aws_cognito_user_pool.main.id
  provider_name = "ms-${each.key}"
  provider_type = "OIDC"

  provider_details = {
    client_id                 = var.microsoft_client_id
    client_secret             = var.microsoft_client_secret
    authorize_scopes          = "openid email profile"
    attributes_request_method = "GET"
    oidc_issuer               = "https://login.microsoftonline.com/${each.value.entra_tenant_id}/v2.0"
    authorize_url             = "https://login.microsoftonline.com/${each.value.entra_tenant_id}/oauth2/v2.0/authorize"
    token_url                 = "https://login.microsoftonline.com/${each.value.entra_tenant_id}/oauth2/v2.0/token"
    attributes_url            = "https://graph.microsoft.com/oidc/userinfo"
    jwks_uri                  = "https://login.microsoftonline.com/${each.value.entra_tenant_id}/discovery/v2.0/keys"
  }

  attribute_mapping = {
    email    = "email"
    username = "sub"
  }
}
