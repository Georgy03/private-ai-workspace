#!/usr/bin/env bash
# Set up or take away a firm, from your own machine with your own AWS sign-in (an administrator).
#
#   ./firm.sh provision "Acme Law LLP" [--microsoft-tenant <their Microsoft tenant id>]
#   ./firm.sh add-microsoft <firm id> <their Microsoft tenant id> [--domain acme.com]...
#   ./firm.sh remove-microsoft <firm id>
#   ./firm.sh add-microsoft-personal   (once: sign-in for @outlook.com and other personal Microsoft accounts)
#   ./firm.sh remove-microsoft-personal
#   ./firm.sh prune-newcomers [--days 30] [--delete]   (lists abandoned sign-ups; deletes only with --delete)
#   ./firm.sh enable-google           (once, after OpenTofu has made the Google provider)
#
# Adding a firm needs no Terraform change: Microsoft sign-in for the firm is made through AWS when you run this.
# It reads backend/.env.neon (database logins) and backend/.env.signin (git-ignored; the one Microsoft sign-in app:
#   MICROSOFT_SIGNIN_CLIENT_ID=...  MICROSOFT_SIGNIN_CLIENT_SECRET=...).
# From backend/.env.railway it takes only the two COGNITO_ lines, never the services' own AWS key.
set -euo pipefail
cd "$(dirname "$0")/.."
export AWS_REGION="${AWS_REGION:-us-east-2}" AWS_PAGER=""
for f in .env.neon .env.railway; do [ -f "$f" ] || { echo "backend/$f is missing."; exit 1; }; done
set -a
. ./.env.neon
[ -f .env.signin ] && . ./.env.signin
eval "$(grep -E '^COGNITO_(ISSUER|CLIENT_ID)=' .env.railway)"
set +a
export DATABASE_URL="postgresql://prism_prov:$NEON_PROV_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?sslmode=require"
export DATABASE_CA_FILE="${DATABASE_CA_FILE:-/etc/ssl/cert.pem}"
case "${1:-}" in
  provision | add-microsoft | remove-microsoft | enable-google | add-microsoft-personal | remove-microsoft-personal | prune-newcomers)
    cmd="$1"; shift
    [ "$cmd" = provision ] && cmd=provision-firm
    exec cargo run -q --bin workspace-cloud -- "$cmd" "$@" ;;
  *) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
