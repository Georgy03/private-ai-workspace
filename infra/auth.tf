# Sign-in. Firms are created by us, not by sign-up, and the firm a person belongs to is an
# attribute that only the server can set.
resource "aws_cognito_user_pool" "main" {
  name                     = local.prefix
  mfa_configuration        = "ON"
  deletion_protection      = "ACTIVE"
  username_attributes      = ["email"]
  auto_verified_attributes = ["email"]

  admin_create_user_config {
    allow_admin_create_user_only = true
  }

  software_token_mfa_configuration {
    enabled = true
  }

  password_policy {
    minimum_length                   = 14
    require_lowercase                = true
    require_uppercase                = true
    require_numbers                  = true
    require_symbols                  = true
    temporary_password_validity_days = 3
  }

  account_recovery_setting {
    recovery_mechanism {
      name     = "verified_email"
      priority = 1
    }
  }

  # Appears in the ID token as "custom:tenant_id". Immutable once set.
  schema {
    name                     = "tenant_id"
    attribute_data_type      = "String"
    developer_only_attribute = false
    mutable                  = false
    required                 = false
    string_attribute_constraints {
      min_length = 36
      max_length = 36
    }
  }
}

resource "aws_cognito_user_pool_client" "desktop" {
  name         = "${local.prefix}-desktop"
  user_pool_id = aws_cognito_user_pool.main.id

  # A desktop app cannot keep a secret. Sign-in uses the code flow with PKCE instead.
  generate_secret                      = false
  allowed_oauth_flows_user_pool_client = true
  allowed_oauth_flows                  = ["code"]
  allowed_oauth_scopes                 = ["openid", "email", "profile"]
  callback_urls                        = var.callback_urls
  logout_urls                          = var.callback_urls
  supported_identity_providers         = ["COGNITO"]
  prevent_user_existence_errors        = "ENABLED"
  enable_token_revocation              = true

  id_token_validity      = 60
  access_token_validity  = 60
  refresh_token_validity = 7
  token_validity_units {
    id_token      = "minutes"
    access_token  = "minutes"
    refresh_token = "days"
  }

  # The firm attribute is readable but deliberately absent from write_attributes, so a user
  # cannot change it even if they tamper with the app.
  read_attributes  = ["email", "email_verified", "custom:tenant_id"]
  write_attributes = ["email"]
}

resource "aws_cognito_user_pool_domain" "main" {
  domain       = "${local.prefix}-${local.account_id}"
  user_pool_id = aws_cognito_user_pool.main.id
}
