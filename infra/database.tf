resource "aws_db_subnet_group" "main" {
  name       = local.prefix
  subnet_ids = aws_subnet.private[*].id
}

resource "aws_db_parameter_group" "main" {
  name_prefix = "${local.prefix}-"
  family      = "postgres${var.postgres_version}"

  parameter {
    name  = "rds.force_ssl"
    value = "1"
  }
  parameter {
    name  = "log_connections"
    value = "1"
  }
  parameter {
    name  = "log_statement"
    value = "ddl"
  }

  lifecycle {
    create_before_destroy = true
  }
}

# One shared Postgres. Per-firm separation is enforced inside it by row-level security
# (backend/migrations/0001_tenancy.sql). pgvector is added with `CREATE EXTENSION vector`.
resource "aws_db_instance" "main" {
  identifier     = local.prefix
  engine         = "postgres"
  engine_version = var.postgres_version
  instance_class = var.db_instance_class

  allocated_storage     = var.db_storage_gb
  max_allocated_storage = var.db_storage_gb * 4
  storage_type          = "gp3"
  storage_encrypted     = true
  kms_key_id            = aws_kms_key.platform.arn

  db_name  = "workspace"
  username = "dbadmin"
  # RDS creates the admin password and keeps it in Secrets Manager. It never appears in this
  # code or in the state file.
  manage_master_user_password   = true
  master_user_secret_kms_key_id = aws_kms_key.platform.arn

  db_subnet_group_name   = aws_db_subnet_group.main.name
  vpc_security_group_ids = [aws_security_group.db.id]
  parameter_group_name   = aws_db_parameter_group.main.name
  publicly_accessible    = false
  multi_az               = var.db_multi_az

  iam_database_authentication_enabled = true
  backup_retention_period             = 14
  copy_tags_to_snapshot               = true
  deletion_protection                 = true
  skip_final_snapshot                 = false
  final_snapshot_identifier           = "${local.prefix}-final"
  auto_minor_version_upgrade          = true
  performance_insights_enabled        = true
  performance_insights_kms_key_id     = aws_kms_key.platform.arn
  enabled_cloudwatch_logs_exports     = ["postgresql", "upgrade"]
}
