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
  description = "AWS services reached privately from inside the VPC. Each costs a few dollars a month per zone."
  type        = set(string)
  default     = ["kms", "secretsmanager"]
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
