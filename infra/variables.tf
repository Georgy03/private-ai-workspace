variable "name" {
  description = "Short project name used in resource names. Placeholder until the company name is chosen."
  type        = string
  default     = "workspace-cloud"
}

variable "environment" {
  type    = string
  default = "prod"
}

variable "region" {
  type    = string
  default = "us-east-2"
}

variable "alert_email" {
  description = "Receives security alerts and budget warnings. Each recipient must confirm a subscription email."
  type        = string

  validation {
    condition     = can(regex("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$", var.alert_email))
    error_message = "alert_email must be an email address."
  }
}

variable "monthly_budget_usd" {
  type    = number
  default = 300
}

variable "vpc_cidr" {
  type    = string
  default = "10.20.0.0/16"
}

variable "interface_endpoints" {
  description = "AWS services reached privately from inside the VPC. Each costs a few dollars a month per zone. Connector tokens only need KMS."
  type        = set(string)
  default     = ["kms"]
}

variable "postgres_version" {
  type    = string
  default = "17"
}

variable "db_instance_class" {
  type    = string
  default = "db.t4g.medium"
}

variable "db_storage_gb" {
  type    = number
  default = 50
}

variable "db_multi_az" {
  description = "Standby in a second zone. Doubles the database cost; turn on once real client data is stored."
  type        = bool
  default     = false
}

variable "audit_retention_days" {
  description = "Audit logs cannot be deleted or overwritten by anyone except the account root for this long."
  type        = number
  default     = 90
}

variable "callback_urls" {
  description = "Where the desktop app receives the sign-in result."
  type        = list(string)
  default     = ["http://127.0.0.1:53682/callback"]
}

variable "google_client_id" {
  description = "Client ID of the Google sign-in app (a web application with only the openid, email and profile scopes). Leave empty to turn Google sign-in off."
  type        = string
  default     = ""
}

variable "google_client_secret" {
  description = "Client secret of the Google sign-in app. Pass it as TF_VAR_google_client_secret: it ends up in the state file, so keep the state encrypted."
  type        = string
  default     = ""
  sensitive   = true
}

variable "self_serve_firms" {
  description = "Let a person with no firm start their own from the app (POST /v1/onboarding/firm). Turning it on gives the API's access key the right to make firm keys and to attach a person to a firm in the user pool, so leave it off until you want sign-up open. The server also needs ONBOARDING_DATABASE_URL."
  type        = bool
  default     = false
}
