#!/usr/bin/env bash
# Runs the backend tests against a throwaway local Postgres cluster.
# With TEST_DATABASE_URL already set (as in CI) it uses that server instead.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -n "${TEST_DATABASE_URL:-}" ]; then
  exec cargo test "$@"
fi

PGBIN="${PGBIN:-/Library/PostgreSQL/18/bin}"
[ -x "$PGBIN/initdb" ] || PGBIN="$(dirname "$(command -v initdb)")"
dir="$(mktemp -d)"
cleanup() { "$PGBIN/pg_ctl" -D "$dir/data" stop -m immediate >/dev/null 2>&1 || true; rm -rf "$dir"; }
trap cleanup EXIT

port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
"$PGBIN/initdb" -D "$dir/data" -U postgres --auth=trust -E UTF8 >/dev/null
"$PGBIN/pg_ctl" -D "$dir/data" -o "-p $port -c listen_addresses=127.0.0.1 -k $dir" -w start >/dev/null
TEST_DATABASE_URL="host=127.0.0.1 port=$port user=postgres dbname=postgres" cargo test "$@"
