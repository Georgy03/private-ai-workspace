output "vpc_id" {
  value = aws_vpc.main.id
}

output "private_subnet_ids" {
  value = aws_subnet.private[*].id
}

output "app_security_group_id" {
  value = aws_security_group.app.id
}

output "data_bucket" {
  value = aws_s3_bucket.data.id
}

output "database_endpoint" {
  value = aws_db_instance.main.address
}

output "database_admin_secret_arn" {
  description = "Where RDS keeps the database admin password."
  value       = aws_db_instance.main.master_user_secret[0].secret_arn
}

output "cognito_user_pool_id" {
  value = aws_cognito_user_pool.main.id
}

output "cognito_desktop_client_id" {
  value = aws_cognito_user_pool_client.desktop.id
}

output "cognito_issuer" {
  description = "The token issuer the backend must require."
  value       = "https://cognito-idp.${var.region}.amazonaws.com/${aws_cognito_user_pool.main.id}"
}

output "tenant_access_role_arn" {
  value = aws_iam_role.tenant_access.arn
}

output "task_role_arns" {
  value = {
    api         = aws_iam_role.api_task.arn
    worker      = aws_iam_role.worker_task.arn
    provisioner = aws_iam_role.provisioner_task.arn
  }
}

output "cognito_hosted_ui_url" {
  description = "The address the desktop app signs in at (its PRIVATE_AI_COGNITO_DOMAIN)."
  value       = "https://${aws_cognito_user_pool_domain.main.domain}.auth.${var.region}.amazoncognito.com"
}

output "identity_provider_redirect_uri" {
  description = "Register this as the redirect URI in the Google and Microsoft sign-in apps."
  value       = "https://${aws_cognito_user_pool_domain.main.domain}.auth.${var.region}.amazoncognito.com/oauth2/idpresponse"
}

output "sign_in_providers" {
  description = "The providers made here. Each firm's Microsoft provider is added by workspace-cloud, not listed here."
  value = concat(
    ["COGNITO"],
    aws_cognito_identity_provider.google[*].provider_name,
  )
}

output "services_iam_user" {
  description = "The IAM user the Railway services use. Make its access key by hand (see README.md)."
  value       = aws_iam_user.services.name
}
