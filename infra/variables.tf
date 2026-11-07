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

variable "microsoft_client_id" {
  description = "Application (client) ID of the Microsoft sign-in app: one multi-tenant registration used for every firm. Leave empty to turn Microsoft sign-in off."
  type        = string
  default     = ""
}

variable "microsoft_client_secret" {
  description = "Client secret of the Microsoft sign-in app. Pass it as TF_VAR_microsoft_client_secret."
  type        = string
  default     = ""
  sensitive   = true
}

variable "microsoft_firms" {
  description = "One entry per firm that signs in with Microsoft. The key is a short name (lowercase letters, digits and hyphens, up to 29); firm_id is the firm's ID in this service; entra_tenant_id is the firm's Microsoft tenant ID."
  type = map(object({
    firm_id         = string
    entra_tenant_id = string
  }))
  default = {}

  validation {
    condition     = alltrue([for name in keys(var.microsoft_firms) : can(regex("^[a-z0-9-]{1,29}$", name))])
    error_message = "Each Microsoft firm name must be 1 to 29 lowercase letters, digits or hyphens."
  }

  validation {
    condition = alltrue([
      for firm in values(var.microsoft_firms) :
      can(regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", firm.firm_id)) &&
      can(regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", firm.entra_tenant_id))
    ])
    error_message = "firm_id and entra_tenant_id must each be a lowercase UUID."
  }
}
