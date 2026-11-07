#!/usr/bin/env bash
# Run this after ./apply-core.sh. It does the steps that follow, in order, and stops at the first problem:
#
#   1. saves the sign-in settings (the user pool's issuer and the desktop app's client ID)
#   2. makes the access key the Railway services use, and saves it without printing it
#   3. makes your first firm and its own encryption key
#   4. tests the real thing: stores, reads back and deletes a test secret under that key
#
# It is safe to run again: anything already saved is skipped. Everything it saves goes into
# backend/.env.railway, which git ignores. It needs your own AWS sign-in (an administrator) to make the key,
# and backend/.env.neon for the database logins.
#
#   ./finish-setup.sh                  names the first firm "Prism Staging"
#   ./finish-setup.sh "Acme Law LLP"   names it as you give
set -euo pipefail
cd "$(dirname "$0")"
export AWS_REGION="${AWS_REGION:-us-east-2}" AWS_PAGER=""
ENV=../backend/.env.railway
NEON=../backend/.env.neon
[ -f "$ENV" ] && [ -f "$NEON" ] || { echo "backend/.env.railway and backend/.env.neon must both exist."; exit 1; }
FIRM_NAME="${1:-Prism Staging}"

PREFIX=workspace-cloud-prod   # the "name" and "environment" settings in variables.tf, joined

# After a partly targeted apply, OpenTofu may not have refreshed every output, so each one falls back to
# asking AWS directly for the same thing.
pool_id() { aws cognito-idp list-user-pools --max-results 60 --query "UserPools[?Name=='$PREFIX'].Id | [0]" --output text | grep -v '^None$'; }
issuer() { tofu output -raw cognito_issuer 2>/dev/null || echo "https://cognito-idp.$AWS_REGION.amazonaws.com/$(pool_id)"; }
client_id() { tofu output -raw cognito_desktop_client_id 2>/dev/null || aws cognito-idp list-user-pool-clients --user-pool-id "$(pool_id)" --query "UserPoolClients[?ClientName=='$PREFIX-desktop'].ClientId | [0]" --output text; }
services_user() { tofu output -raw services_iam_user 2>/dev/null || echo "$PREFIX-services"; }
hosted_ui() { tofu output -raw cognito_hosted_ui_url 2>/dev/null || echo "https://$PREFIX-$(aws sts get-caller-identity --query Account --output text).auth.$AWS_REGION.amazoncognito.com"; }

have() { grep -q "^$1=" "$ENV"; }
put() { printf '%s=%s\n' "$1" "$2" >> "$ENV"; echo "   saved $1"; }

echo "1. Sign-in settings"
if ! pool_id >/dev/null; then
  echo "   The user pool does not exist yet. Run ./apply-core.sh first."; exit 1
fi
have COGNITO_ISSUER || put COGNITO_ISSUER "$(issuer)"
have COGNITO_CLIENT_ID || put COGNITO_CLIENT_ID "$(client_id)"
echo "   The desktop app's sign-in address (PRIVATE_AI_COGNITO_DOMAIN): $(hosted_ui)"

echo "2. The services' access key"
if have AWS_ACCESS_KEY_ID; then
  echo "   already saved"
else
  aws iam create-access-key --user-name "$(services_user)" \
    --query 'AccessKey.[AccessKeyId,SecretAccessKey]' --output text \
    | awk '{printf "AWS_ACCESS_KEY_ID=%s\nAWS_SECRET_ACCESS_KEY=%s\n",$1,$2}' >> "$ENV"
  echo "   saved AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY (not shown)"
fi

echo "3. First firm and its encryption key (using your own AWS sign-in)"
if have FIRM_ID; then
  echo "   already made"
else
  # Your own sign-in makes the key, so .env.railway (the services' limited key) is deliberately not loaded here.
  MADE=$(
    set -a; . "$NEON"; set +a
    cd ../backend
    DATABASE_URL="postgresql://prism_prov:$NEON_PROV_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?sslmode=require" \
      DATABASE_CA_FILE="${DATABASE_CA_FILE:-/etc/ssl/cert.pem}" \
      cargo run -q --bin workspace-cloud -- provision-firm "$FIRM_NAME"
  )
  FIRM_ID=$(printf '%s\n' "$MADE" | sed -n 's/^tenant_id=//p' | head -1)
  KEY_ARN=$(printf '%s\n' "$MADE" | sed -n 's/^kms_key_arn=//p' | head -1)
  [ -n "$FIRM_ID" ] && [ -n "$KEY_ARN" ] || { echo "   The firm was not made; the command said:"; printf '%s\n' "$MADE"; exit 1; }
  put FIRM_ID "$FIRM_ID"
  put FIRM_KEY_ARN "$KEY_ARN"
fi

echo "4. Testing the real token store (the services' key, the API's database login, real AWS)"
# A new access key can take a few seconds to work everywhere.
sleep 10
(
  set -a; . "$NEON"; . "$ENV"; set +a
  cd ../backend
  DATABASE_URL="postgresql://prism_api:$NEON_API_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?sslmode=require" \
    DATABASE_CA_FILE="${DATABASE_CA_FILE:-/etc/ssl/cert.pem}" \
    cargo run -q --bin workspace-cloud -- check-secrets "$FIRM_ID"
)
echo
echo "Done. Next: invite yourself (README, \"To invite a person\") with firm ID $(grep '^FIRM_ID=' "$ENV" | cut -d= -f2),"
echo "then put the settings on Railway with backend/scripts/railway-vars.sh."
