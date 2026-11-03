# Per-firm separation for storage, keys and secrets (the database is separated by row-level
# security). The idea: the service never uses its own broad role to touch firm data. It assumes
# `tenant_access` with a session tag naming the firm, and every permission below is written
# against that tag. A bug that asks for another firm's file is refused by AWS itself.
#
# Runtime conventions the services must follow:
#   files    s3://<data bucket>/tenants/<firm id>/...      uploaded with SSE-KMS and that firm's key
#   keys     one KMS key per firm, tagged tenant=<firm id>
#   secrets  tenants/<firm id>/<name>
#   session  sts:AssumeRole on tenant_access with the session tag tenant=<firm id>

data "aws_iam_policy_document" "ecs_tasks_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
  }
}

# Roles the containers run as. They hold almost nothing themselves: their one important
# permission is to become `tenant_access` for a named firm.
resource "aws_iam_role" "api_task" {
  name               = "${local.prefix}-api"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

resource "aws_iam_role" "worker_task" {
  name               = "${local.prefix}-worker"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

resource "aws_iam_role" "provisioner_task" {
  name               = "${local.prefix}-provisioner"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

# ---- tenant_access: the only role that touches a firm's files, keys and secrets ----

data "aws_iam_policy_document" "tenant_access_trust" {
  statement {
    actions = ["sts:AssumeRole", "sts:TagSession"]
    principals {
      type        = "AWS"
      identifiers = [aws_iam_role.api_task.arn, aws_iam_role.worker_task.arn]
    }
    # A session without a firm tag is refused outright.
    condition {
      test     = "Null"
      variable = "aws:RequestTag/tenant"
      values   = ["false"]
    }
    # The tag must look like a firm ID (a UUID), so wildcards and path tricks are rejected.
    condition {
      test     = "StringLike"
      variable = "aws:RequestTag/tenant"
      values   = ["????????-????-????-????-????????????"]
    }
  }
}

resource "aws_iam_role" "tenant_access" {
  name                 = "${local.prefix}-tenant-access"
  assume_role_policy   = data.aws_iam_policy_document.tenant_access_trust.json
  max_session_duration = 3600
}

data "aws_iam_policy_document" "tenant_access" {
  statement {
    sid       = "OwnFilesOnly"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.data.arn}/tenants/&{aws:PrincipalTag/tenant}/*"]
  }

  statement {
    sid       = "ListOwnPrefixOnly"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.data.arn]
    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["tenants/&{aws:PrincipalTag/tenant}/*"]
    }
  }

  statement {
    sid       = "OwnKeyOnly"
    actions   = ["kms:Decrypt", "kms:Encrypt", "kms:GenerateDataKey", "kms:DescribeKey"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/tenant"
      values   = ["&{aws:PrincipalTag/tenant}"]
    }
  }

  statement {
    sid = "OwnSecretsOnly"
    actions = [
      "secretsmanager:GetSecretValue",
      "secretsmanager:PutSecretValue",
      "secretsmanager:CreateSecret",
      "secretsmanager:DeleteSecret",
    ]
    resources = [
      "arn:${local.partition}:secretsmanager:${var.region}:${local.account_id}:secret:tenants/&{aws:PrincipalTag/tenant}/*",
    ]
  }
}

resource "aws_iam_role_policy" "tenant_access" {
  name   = "own-firm-only"
  role   = aws_iam_role.tenant_access.id
  policy = data.aws_iam_policy_document.tenant_access.json
}

data "aws_iam_policy_document" "assume_tenant_access" {
  statement {
    actions   = ["sts:AssumeRole", "sts:TagSession"]
    resources = [aws_iam_role.tenant_access.arn]
  }
}

resource "aws_iam_role_policy" "api_assume" {
  name   = "assume-tenant-access"
  role   = aws_iam_role.api_task.id
  policy = data.aws_iam_policy_document.assume_tenant_access.json
}

resource "aws_iam_role_policy" "worker_assume" {
  name   = "assume-tenant-access"
  role   = aws_iam_role.worker_task.id
  policy = data.aws_iam_policy_document.assume_tenant_access.json
}

# ---- provisioner: creates and removes a firm's key, files and secrets, and nothing else ----

data "aws_iam_policy_document" "provisioner" {
  statement {
    sid       = "CreateOnlyTaggedKeys"
    actions   = ["kms:CreateKey", "kms:TagResource"]
    resources = ["*"]
    condition {
      test     = "Null"
      variable = "aws:RequestTag/tenant"
      values   = ["false"]
    }
  }

  statement {
    sid       = "ManageFirmKeys"
    actions   = ["kms:CreateAlias", "kms:DescribeKey", "kms:EnableKeyRotation", "kms:ScheduleKeyDeletion"]
    resources = ["*"]
    condition {
      test     = "Null"
      variable = "aws:ResourceTag/tenant"
      values   = ["false"]
    }
  }

  statement {
    sid       = "AliasesForFirmKeys"
    actions   = ["kms:CreateAlias", "kms:DeleteAlias"]
    resources = ["arn:${local.partition}:kms:${var.region}:${local.account_id}:alias/tenants/*"]
  }

  statement {
    sid       = "RemoveFirmFiles"
    actions   = ["s3:DeleteObject", "s3:DeleteObjectVersion"]
    resources = ["${aws_s3_bucket.data.arn}/tenants/*"]
  }

  statement {
    sid       = "ListFirmFiles"
    actions   = ["s3:ListBucket", "s3:ListBucketVersions"]
    resources = [aws_s3_bucket.data.arn]
  }

  statement {
    sid       = "RemoveFirmSecrets"
    actions   = ["secretsmanager:DeleteSecret", "secretsmanager:ListSecrets"]
    resources = ["arn:${local.partition}:secretsmanager:${var.region}:${local.account_id}:secret:tenants/*"]
  }
}

resource "aws_iam_role_policy" "provisioner" {
  name   = "firm-lifecycle"
  role   = aws_iam_role.provisioner_task.id
  policy = data.aws_iam_policy_document.provisioner.json
}
