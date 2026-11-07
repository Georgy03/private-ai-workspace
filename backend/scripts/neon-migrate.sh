#!/usr/bin/env bash
# Applies the database migrations to the Neon database safely.
#
# Migrations that are still to run are first tried on a temporary copy of the production database (a
# Neon branch). Only if that works, and the application logins can still use the result, are they run
# on production. The copy is deleted when the script ends, and it is also set to expire by itself in
# an hour in case this script is stopped half way.
#
#   scripts/neon-migrate.sh               test on a copy, then migrate production if anything is pending
#   scripts/neon-migrate.sh --test-only   copy, migrate the copy, check it, delete it; production is not touched
#
# Needs backend/.env.neon (the owner login, NEON_PROJECT_ID, and the service logins) and the Neon CLI,
# signed in (neon auth). To rehearse an upgrade that is not pending, set NEON_BRANCH_PREP_SQL to SQL
# that undoes it on the copy, for example:
#   NEON_BRANCH_PREP_SQL='DROP TABLE zoom_meetings; DELETE FROM schema_migrations WHERE version = 24' \
#     scripts/neon-migrate.sh --test-only
#
# NEON_MIGRATE_SKIP_TEST=1 migrates production directly, without the test on a copy. Use it only when
# you cannot reach the Neon CLI, and say so to whoever else works on this database.
set -euo pipefail
cd "$(dirname "$0")/.."

TEST_ONLY=0
[ "${1:-}" = "--test-only" ] && TEST_ONLY=1
say() { printf '%s\n' "$*"; }
fail() { printf '%s\n' "$*" >&2; exit 1; }

[ -f .env.neon ] || fail "backend/.env.neon is missing (the Neon addresses and logins)."
set -a; . ./.env.neon; set +a
: "${NEON_OWNER_URL:?missing in .env.neon}" "${NEON_PROJECT_ID:?add NEON_PROJECT_ID to .env.neon}" "${NEON_DB:?missing}" \
  "${NEON_API_PASSWORD:?missing}"
PARENT="${NEON_BRANCH:-production}"
export DATABASE_CA_FILE="${DATABASE_CA_FILE:-/etc/ssl/cert.pem}"
PGBIN="${PGBIN:-/Library/PostgreSQL/18/bin}"
[ -x "$PGBIN/psql" ] || PGBIN="$(dirname "$(command -v psql)")"
SSL="sslmode=verify-full&sslrootcert=system"

cargo build -q --bin workspace-cloud
BIN=target/debug/workspace-cloud

# What the code expects, and what production already has.
LATEST=$(ls migrations/*.sql | sed -E 's#.*/0*([0-9]+)_.*#\1#' | sort -n | tail -1)
version_in() { "$PGBIN/psql" "$1&$SSL" -Atc "select case when to_regclass('public.schema_migrations') is null then 0 else (select coalesce(max(version), 0) from schema_migrations) end"; }
APPLIED=$(version_in "$NEON_OWNER_URL")
if [ "$APPLIED" -ge "$LATEST" ] && [ "$TEST_ONLY" = 0 ]; then
  say "Neon is up to date (migration $APPLIED)."; exit 0
fi
say "Production has migration $APPLIED; the code has $LATEST."

if [ "${NEON_MIGRATE_SKIP_TEST:-0}" = 1 ] && [ "$TEST_ONLY" = 0 ]; then
  say "NEON_MIGRATE_SKIP_TEST is set: migrating production without a test on a copy."
  DATABASE_URL="$NEON_OWNER_URL" $BIN migrate
  say "Production is now at migration $(version_in "$NEON_OWNER_URL")."; exit 0
fi
command -v neon >/dev/null || fail "The Neon CLI is not installed, so the test on a copy cannot run. Install it, or set NEON_MIGRATE_SKIP_TEST=1."
neon me >/dev/null 2>&1 || fail "The Neon CLI is not signed in. Run: neon auth"

# ---- a temporary copy of production ----
NAME="migrate-test-$(date +%Y%m%d-%H%M%S)"
EXPIRES=$(date -u -v+1H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)
CREATED=0
cleanup() {
  if [ "$CREATED" = 1 ]; then
    neon branches delete "$NAME" --project-id "$NEON_PROJECT_ID" >/dev/null 2>&1 && say "Deleted the temporary copy ($NAME)." || say "Could not delete the copy $NAME. It expires by itself at $EXPIRES."
  fi
}
trap cleanup EXIT
say "Making a temporary copy of '$PARENT' ($NAME)..."
neon branches create --project-id "$NEON_PROJECT_ID" --name "$NAME" --parent "$PARENT" --expires-at "$EXPIRES" --no-secrets >/dev/null
CREATED=1
BRANCH_URL=$(neon connection-string "$NAME" --project-id "$NEON_PROJECT_ID" --role-name neondb_owner --database-name "$NEON_DB" --ssl require)
BRANCH_HOST=$(python3 -c 'import sys,urllib.parse as u; print(u.urlparse(sys.argv[1]).hostname)' "$BRANCH_URL")
for _ in $(seq 1 30); do "$PGBIN/psql" "$BRANCH_URL" -Atc "select 1" >/dev/null 2>&1 && break; sleep 2; done
"$PGBIN/psql" "$BRANCH_URL" -Atc "select 1" >/dev/null 2>&1 || fail "The temporary copy did not come up. Production was not touched."

if [ -n "${NEON_BRANCH_PREP_SQL:-}" ]; then
  say "Rehearsing an upgrade: undoing it on the copy first."
  "$PGBIN/psql" "$BRANCH_URL" -q -v ON_ERROR_STOP=1 -c "$NEON_BRANCH_PREP_SQL" || fail "The preparation SQL failed on the copy. Production was not touched."
  say "The copy is now at migration $(version_in "$BRANCH_URL")."
fi

say "Running the migrations on the copy..."
DATABASE_URL="$BRANCH_URL" $BIN migrate >/dev/null 2>&1 || { DATABASE_URL="$BRANCH_URL" $BIN migrate 2>&1 | tail -3 | sed 's#postgres[^ ]*@#postgres...@#' || true; fail "The migrations failed on the copy. Production was not touched."; }
AFTER=$(version_in "$BRANCH_URL")
[ "$AFTER" = "$LATEST" ] || fail "The copy ended at migration $AFTER, not $LATEST. Production was not touched."

# The application logins must still work on the result: a migration that forgets a permission would
# break the API only after it was live.
API_URL="postgresql://prism_api:$NEON_API_PASSWORD@$BRANCH_HOST/$NEON_DB?$SSL"
"$PGBIN/psql" "$API_URL" -q -v ON_ERROR_STOP=1 <<'SQL' >/dev/null 2>&1 || fail "The API login cannot use the migrated copy. Production was not touched."
select count(*) from documents;
select count(*) from chunks;
select count(*) from connections;
select count(*) from zoom_meetings;
select count(*) from connector_secrets;
SQL
if [ -n "${NEON_WORKER_PASSWORD:-}" ]; then
  "$PGBIN/psql" "postgresql://prism_worker:$NEON_WORKER_PASSWORD@$BRANCH_HOST/$NEON_DB?$SSL" -q -v ON_ERROR_STOP=1 -c "select count(*) from sync_queue; select count(*) from connector_secrets" >/dev/null 2>&1 || fail "The worker login cannot use the migrated copy. Production was not touched."
fi
say "The copy passed: migrated to $AFTER, and the API and worker logins still work."

if [ "$TEST_ONLY" = 1 ]; then
  say "Test only: production was not changed."; exit 0
fi
say "Migrating production..."
DATABASE_URL="$NEON_OWNER_URL" $BIN migrate >/dev/null 2>&1 || { DATABASE_URL="$NEON_OWNER_URL" $BIN migrate 2>&1 | tail -3 | sed 's#postgres[^ ]*@#postgres...@#' || true; fail "The migration failed on production after passing on the copy. Check the database before starting the services."; }
say "Production is now at migration $(version_in "$NEON_OWNER_URL")."
