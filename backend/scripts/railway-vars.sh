#!/usr/bin/env bash
# Sets the staging variables on the Railway services `api` and `worker`. Run it yourself, from a terminal
# signed in to Railway (railway login) and linked to the project (railway link), so secrets go straight
# from your private files into Railway and are never printed.
#
# Reads two git-ignored files in backend/:
#   .env.neon     the Neon addresses and the three service logins (already there)
#   .env.railway  the rest: copy railway.env.example to .env.railway and fill it in
#
# It prints variable NAMES only. It sets nothing that is empty, and refuses ALLOW_INSECURE.
#   scripts/railway-vars.sh            set everything it can, without redeploying
#   scripts/railway-vars.sh --deploy   same, then redeploy both services
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f .env.neon ] || { echo "backend/.env.neon is missing."; exit 1; }
[ -f .env.railway ] || { echo "Copy railway.env.example to .env.railway and fill it in first."; exit 1; }
set -a; . ./.env.neon; . ./.env.railway; set +a
[ -z "${ALLOW_INSECURE:-}" ] || { echo "ALLOW_INSECURE must not be set on Railway."; exit 1; }
[ -z "${ANTHROPIC_API_KEY:-}" ] || { echo "ANTHROPIC_API_KEY must not be set on Railway: the server refuses the direct Claude API outside development."; exit 1; }
for needed in NEON_POOLED_HOST NEON_DB NEON_API_PASSWORD NEON_WORKER_PASSWORD; do
  [ -n "${!needed:-}" ] || { echo "$needed is missing from .env.neon."; exit 1; }
done

# One signing key for both services, kept in .env.railway so a rerun does not change it.
if [ -z "${OAUTH_STATE_KEY:-}" ]; then
  OAUTH_STATE_KEY=$(openssl rand -base64 32 | tr -d '\n')
  printf '\nOAUTH_STATE_KEY=%s\n' "$OAUTH_STATE_KEY" >> .env.railway
  echo "Made a new OAUTH_STATE_KEY and saved it in .env.railway."
fi

set_var() { # service name value
  [ -n "$3" ] || return 0
  printf '%s' "$3" | railway variable set "$2" --stdin --skip-deploys --service "$1" >/dev/null
  echo "  $1: $2"
}
db_url() { echo "postgresql://$1:$2@$NEON_POOLED_HOST/$NEON_DB?sslmode=require"; }

COMMON=(OAUTH_STATE_KEY OAUTH_REDIRECT_URI AWS_REGION AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY
  MICROSOFT_CLIENT_ID MICROSOFT_CLIENT_SECRET GOOGLE_CLIENT_ID GOOGLE_CLIENT_SECRET
  DROPBOX_CLIENT_ID DROPBOX_CLIENT_SECRET DOCUSIGN_CLIENT_ID DOCUSIGN_CLIENT_SECRET DOCUSIGN_ENVIRONMENT
  QUICKBOOKS_CLIENT_ID QUICKBOOKS_CLIENT_SECRET QUICKBOOKS_ENVIRONMENT ZOOM_CLIENT_ID ZOOM_CLIENT_SECRET)
API_ONLY=(COGNITO_ISSUER COGNITO_CLIENT_ID)

echo "api:"
set_var api DATABASE_URL "$(db_url prism_api "$NEON_API_PASSWORD")"
for name in "${COMMON[@]}" "${API_ONLY[@]}"; do set_var api "$name" "${!name:-}"; done
echo "worker:"
set_var worker DATABASE_URL "$(db_url prism_worker "$NEON_WORKER_PASSWORD")"
for name in "${COMMON[@]}"; do set_var worker "$name" "${!name:-}"; done

if [ "${1:-}" = "--deploy" ]; then
  railway service redeploy --service api --yes
  railway service redeploy --service worker --yes
else
  echo "Done. Nothing was redeployed; run with --deploy, or redeploy from the Railway dashboard."
fi
