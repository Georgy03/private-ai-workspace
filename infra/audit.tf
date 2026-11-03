locals {
  trail_name = "${local.prefix}-trail"
  trail_arn  = "arn:${local.partition}:cloudtrail:${var.region}:${local.account_id}:trail/${local.trail_name}"
}

resource "aws_cloudtrail" "main" {
  name                          = local.trail_name
  s3_bucket_name                = aws_s3_bucket.audit.id
  kms_key_id                    = aws_kms_key.audit.arn
  is_multi_region_trail         = true
  include_global_service_events = true
  enable_log_file_validation    = true

  # Who read or wrote which firm file. This is the evidence a firm asks for after an incident.
  event_selector {
    read_write_type           = "All"
    include_management_events = true
    data_resource {
      type   = "AWS::S3::Object"
      values = ["${aws_s3_bucket.data.arn}/"]
    }
  }

  depends_on = [aws_s3_bucket_policy.audit]
}

resource "aws_guardduty_detector" "main" {
  enable = true
}

resource "aws_guardduty_detector_feature" "s3" {
  detector_id = aws_guardduty_detector.main.id
  name        = "S3_DATA_EVENTS"
  status      = "ENABLED"
}

resource "aws_guardduty_detector_feature" "rds" {
  detector_id = aws_guardduty_detector.main.id
  name        = "RDS_LOGIN_EVENTS"
  status      = "ENABLED"
}

# ---- Alerts ----

resource "aws_sns_topic" "security" {
  name              = "${local.prefix}-security"
  kms_master_key_id = "alias/aws/sns"
}

resource "aws_sns_topic_subscription" "security_email" {
  topic_arn = aws_sns_topic.security.arn
  protocol  = "email"
  endpoint  = var.alert_email
}

data "aws_iam_policy_document" "security_topic" {
  statement {
    sid       = "EventBridgePublishes"
    actions   = ["sns:Publish"]
    resources = [aws_sns_topic.security.arn]
    principals {
      type        = "Service"
      identifiers = ["events.amazonaws.com"]
    }
  }
}

resource "aws_sns_topic_policy" "security" {
  arn    = aws_sns_topic.security.arn
  policy = data.aws_iam_policy_document.security_topic.json
}

resource "aws_cloudwatch_event_rule" "guardduty" {
  name        = "${local.prefix}-guardduty-findings"
  description = "GuardDuty findings of medium severity or higher"
  event_pattern = jsonencode({
    source        = ["aws.guardduty"]
    "detail-type" = ["GuardDuty Finding"]
    detail        = { severity = [{ numeric = [">=", 4] }] }
  })
}

resource "aws_cloudwatch_event_target" "guardduty" {
  rule = aws_cloudwatch_event_rule.guardduty.name
  arn  = aws_sns_topic.security.arn
}

# Console sign-in events are regional. If the root user could sign in through another region's
# endpoint, add the same rule there.
resource "aws_cloudwatch_event_rule" "root_login" {
  name        = "${local.prefix}-root-login"
  description = "The account root user signed in"
  event_pattern = jsonencode({
    "detail-type" = ["AWS Console Sign In via CloudTrail"]
    detail        = { userIdentity = { type = ["Root"] } }
  })
}

resource "aws_cloudwatch_event_target" "root_login" {
  rule = aws_cloudwatch_event_rule.root_login.name
  arn  = aws_sns_topic.security.arn
}

resource "aws_budgets_budget" "monthly" {
  name         = "${local.prefix}-monthly"
  budget_type  = "COST"
  limit_amount = tostring(var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.alert_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = [var.alert_email]
  }
}
