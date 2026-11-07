#!/usr/bin/env bash
# Creates only what the first deployment needs: sign-in (the user pool, its app, the administrators group, the
# Google provider and the pre sign-up function) and the limited IAM user the Railway services
# use for connector tokens. It does NOT create the network (with its NAT gateway), the RDS database, the
# buckets or the audit tools that the rest of this folder describes; the database is Neon and those can wait.
#
#   ./apply-core.sh            shows the plan and asks before changing anything
#   ./apply-core.sh -auto-approve
set -euo pipefail
cd "$(dirname "$0")"
exec tofu apply \
  -target=aws_cognito_user_pool.main \
  -target=aws_cognito_user_pool_client.desktop \
  -target=aws_cognito_user_pool_domain.main \
  -target=aws_cognito_user_group.firm_admin \
  -target=aws_cognito_identity_provider.google \
  -target=aws_lambda_function.presignup \
  -target=aws_lambda_permission.presignup \
  -target=aws_iam_role_policy.presignup \
  -target=aws_iam_user_policy.services_token_keys \
  -target=aws_iam_user_policy.services_sign_in_lookup \
  "$@"
