# Per-firm separation for storage and keys (the database is separated by row-level security). The
# idea: the service never uses its own broad role to touch a firm's files. It assumes `tenant_access`
# with a session tag naming the firm, and every permission below is written against that tag. A bug
# that asks for another firm's file is refused by AWS itself.
#
# Connector tokens are rows in the database, encrypted under each firm's own KMS key (backend/src/
# kms.rs, backend/migrations/0025_connector_secrets.sql); there is no Secrets Manager for them. The API
# and worker task roles use the firm keys directly (`token_keys` below), limited to keys that carry the
# application tag and to calls whose encryption context names the firm and the secret. KMS refuses to
# open a stored token under any other context. Assuming `tenant_access` for those calls too would make
# AWS itself refuse a key used for the wrong firm; that is a possible later tightening.
#
# Runtime conventions the services must follow:
#   files    s3://<data bucket>/tenants/<firm id>/...      uploaded with SSE-KMS and that firm's key
#   keys     one KMS key per firm, tagged app=prism-workspace and tenant=<firm id> (and firm=<name>),
#            with the alias alias/prism-tenant-<firm id> and yearly rotation on
#   tokens   encrypted rows in the database; encryption context {tenant_id, secret}
#   session  sts:AssumeRole on tenant_access with the session tag tenant=<firm id> (files)

locals {
  # Must match backend/src/kms.rs APP_TAG and the encryption context keys in Binding::context.
  app_tag = "prism-workspace"
}

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

# ---- connector tokens: the API stores them, the worker reads and replaces them ----

data "aws_iam_policy_document" "token_keys" {
  statement {
    sid       = "UseFirmKeysForTokens"
    actions   = ["kms:GenerateDataKey", "kms:Decrypt"]
    resources = ["arn:${local.partition}:kms:${var.region}:${local.account_id}:key/*"]
    # Only keys made for this application, never the platform or audit key or anything else.
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/app"
      values   = [local.app_tag]
    }
    # Every call names the firm and the secret, and nothing else, so KMS can refuse a mismatch.
    condition {
      test     = "Null"
      variable = "kms:EncryptionContext:tenant_id"
      values   = ["false"]
    }
    condition {
      test     = "Null"
      variable = "kms:EncryptionContext:secret"
      values   = ["false"]
    }
    condition {
      test     = "ForAllValues:StringEquals"
      variable = "kms:EncryptionContextKeys"
      values   = ["tenant_id", "secret"]
    }
  }
}

resource "aws_iam_role_policy" "api_token_keys" {
  name   = "connector-token-keys"
  role   = aws_iam_role.api_task.id
  policy = data.aws_iam_policy_document.token_keys.json
}

resource "aws_iam_role_policy" "worker_token_keys" {
  name   = "connector-token-keys"
  role   = aws_iam_role.worker_task.id
  policy = data.aws_iam_policy_document.token_keys.json
}

# ---- provisioner: creates and removes a firm's key and files, and nothing else ----
# It runs `workspace-cloud provision-firm`, which makes the key (tagged, aliased, rotating) and then
# the firm. It can neither use a key to encrypt or decrypt, nor read a token.

data "aws_iam_policy_document" "provisioner" {
  statement {
    sid       = "CreateOnlyOurKeys"
    actions   = ["kms:CreateKey", "kms:TagResource"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:RequestTag/app"
      values   = [local.app_tag]
    }
    condition {
      test     = "Null"
      variable = "aws:RequestTag/tenant"
      values   = ["false"]
    }
    condition {
      test     = "ForAllValues:StringEquals"
      variable = "aws:TagKeys"
      values   = ["app", "tenant", "firm"]
    }
  }

  statement {
    sid       = "ManageFirmKeys"
    actions   = ["kms:CreateAlias", "kms:DescribeKey", "kms:EnableKeyRotation", "kms:ScheduleKeyDeletion"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:ResourceTag/app"
      values   = [local.app_tag]
    }
  }

  statement {
    sid       = "AliasesForFirmKeys"
    actions   = ["kms:CreateAlias", "kms:DeleteAlias"]
    resources = ["arn:${local.partition}:kms:${var.region}:${local.account_id}:alias/prism-tenant-*"]
  }

  # A firm's Microsoft sign-in (`workspace-cloud provision-firm --microsoft-tenant`, `add-microsoft`,
  # `remove-microsoft`): the provider, the record of which firm it belongs to, and the app's list of providers.
  statement {
    sid = "ManageFirmMicrosoftSignIn"
    actions = [
      "cognito-idp:CreateIdentityProvider",
      "cognito-idp:DeleteIdentityProvider",
      "cognito-idp:DescribeUserPoolClient",
      "cognito-idp:UpdateUserPoolClient",
    ]
    resources = [aws_cognito_user_pool.main.arn]
  }

  statement {
    sid       = "RecordWhichFirmAProviderBelongsTo"
    actions   = ["ssm:PutParameter", "ssm:DeleteParameter", "ssm:GetParametersByPath"]
    resources = ["arn:${local.partition}:ssm:${var.region}:${local.account_id}:parameter${local.firm_parameter_path}*"]
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
}

resource "aws_iam_role_policy" "provisioner" {
  name   = "firm-lifecycle"
  role   = aws_iam_role.provisioner_task.id
  policy = data.aws_iam_policy_document.provisioner.json
}
