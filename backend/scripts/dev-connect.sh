#!/usr/bin/env bash
# Try a connector against the real Microsoft or Google sign-in, on this machine only.
#
# Starts a throwaway Postgres, the API and the worker, and a stand-in for the app's own sign-in,
# then sends you to the provider to approve access. When you are back, it waits for the first
# sync and lets you search your files. Everything is deleted when you quit.
#
# Needs backend/.env.local with MICROSOFT_CLIENT_ID and MICROSOFT_CLIENT_SECRET (for microsoft)
# or GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET (for google), and
# http://localhost:8080/oauth/callback registered as a redirect URI on that app.
#
#   scripts/dev-connect.sh microsoft           interactive
#   scripts/dev-connect.sh google
#   scripts/dev-connect.sh google --check      only check the client ID, then exit
#   scripts/dev-connect.sh ui                  starts the server with Google and Microsoft set up (from
#                                              .env.local) and connects nothing: add sources from the
#                                              app's Sources > Connectors tab
#   scripts/dev-connect.sh sample              no provider: three sample cloud files, for trying
#                                              the desktop app (needs no client ID or secret)
#
# While it runs it writes backend/.env.devcloud so `npm run tauri:dev:cloud` can point the desktop
# app at this server. The file is deleted when the script ends.
#
# Add --neon (with `ui`) to use the Neon database in backend/.env.neon instead of a throwaway one: the
# migrations run there, the API runs as prism_api and the worker as prism_worker through Neon's pooled
# address, over verified TLS. The data stays in Neon between runs, so --neon implies --keep for the
# firm identity and the saved access on this machine (backend/.dev-state).
#
# By default everything is deleted when the script stops. Add --keep to keep the database, the
# saved Google/Microsoft access and the firm identity in backend/.dev-state between runs, so a
# restart does not lose a sync. That leaves a plain-text copy of your synced files on this disk;
# remove it any time with:   scripts/dev-connect.sh forget
set -euo pipefail
cd "$(dirname "$0")/.."

[ -f .env.local ] && { set -a; . ./.env.local; set +a; }
STATE="$PWD/.dev-state"
if [ "${1:-}" = "forget" ]; then
  if lsof -nP -iTCP:8080 -sTCP:LISTEN >/dev/null 2>&1; then echo "Stop the server first, then run this again."; exit 1; fi
  rm -rf "$STATE" && echo "Deleted the kept data (backend/.dev-state)."; exit 0
fi
case "${1:-}" in
  microsoft) KIND=microsoft365; PROVIDER=Microsoft
    : "${MICROSOFT_CLIENT_ID:?set MICROSOFT_CLIENT_ID in backend/.env.local}"
    : "${MICROSOFT_CLIENT_SECRET:?set MICROSOFT_CLIENT_SECRET in backend/.env.local}" ;;
  google) KIND=google_drive; PROVIDER=Google
    : "${GOOGLE_CLIENT_ID:?set GOOGLE_CLIENT_ID in backend/.env.local}"
    : "${GOOGLE_CLIENT_SECRET:?set GOOGLE_CLIENT_SECRET in backend/.env.local}" ;;
  sample) KIND=""; PROVIDER="Sample"; SEED=1 ;;
  ui) KIND=""; PROVIDER="UI"; SEED=0 ;;
  *) echo "usage: scripts/dev-connect.sh microsoft|google|ui|sample [--check] [--keep]   |   forget"; exit 2 ;;
esac
CHECK_ONLY=0; KEEP="${DEV_KEEP:-0}"; NEON=0
for arg in "${@:2}"; do
  [ "$arg" = "--check" ] && CHECK_ONLY=1
  [ "$arg" = "--keep" ] && KEEP=1
  [ "$arg" = "--neon" ] && NEON=1
done
if [ "$NEON" = 1 ]; then
  [ "$KIND" = "" ] && [ "$SEED" = 0 ] || { echo "--neon works with the ui mode only."; exit 2; }
  [ -f .env.neon ] || { echo "backend/.env.neon is missing (the Neon addresses and logins)."; exit 1; }
  set -a; . ./.env.neon; set +a
  : "${NEON_OWNER_URL:?}" "${NEON_POOLED_HOST:?}" "${NEON_DB:?}" "${NEON_API_PASSWORD:?}" "${NEON_WORKER_PASSWORD:?}" "${NEON_PROV_PASSWORD:?}"
  KEEP=1
fi
API_PORT=8080
if lsof -nP -iTCP:$API_PORT -sTCP:LISTEN >/dev/null 2>&1; then
  echo "Port $API_PORT is already in use. Stop whatever is using it and try again."; exit 1
fi

cargo build -q
PGBIN="${PGBIN:-/Library/PostgreSQL/18/bin}"
[ -x "$PGBIN/initdb" ] || PGBIN="$(dirname "$(command -v initdb)")"
SP="$(mktemp -d)"
PIDS=()
if [ "$KEEP" = 1 ]; then
  mkdir -p "$STATE"; chmod 700 "$STATE"
  DATA="$STATE/pg"; SECRETS="$STATE/secrets"; STOP_MODE=fast
  echo "Keeping data in backend/.dev-state between runs (remove it with: scripts/dev-connect.sh forget)."
else
  DATA="$SP/data"; SECRETS="$SP/secrets"; STOP_MODE=immediate
fi
cleanup() {
  rm -f .env.devcloud
  for pid in "${PIDS[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; done
  [ "$NEON" = 1 ] || "$PGBIN/pg_ctl" -D "$DATA" stop -m "$STOP_MODE" >/dev/null 2>&1 || true
  rm -rf "$SP"
}
trap cleanup EXIT
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

# ---- database ----
BIN=target/debug/workspace-cloud
if [ "$NEON" = 1 ]; then
  # Neon: the owner login only runs the migrations, over the direct address. The running services use
  # their own limited logins through the pooled address, and the database certificate is checked.
  NEON_SSL="sslmode=verify-full&sslrootcert=system"
  export DATABASE_CA_FILE="${DATABASE_CA_FILE:-/etc/ssl/cert.pem}"
  neon_url() { echo "postgresql://$1:$2@$NEON_POOLED_HOST/$NEON_DB?sslmode=require"; }
  API_DB_URL=$(neon_url prism_api "$NEON_API_PASSWORD"); WORKER_DB_URL=$(neon_url prism_worker "$NEON_WORKER_PASSWORD")
  $BIN download-model >/dev/null 2>&1 || echo "Note: the meaning-search model could not be downloaded; search will match words only."
  # Pending migrations are tried on a temporary copy of the database before production (scripts/neon-migrate.sh).
  scripts/neon-migrate.sh >"$SP/neon-migrate.log" 2>&1 || { echo "The migrations could not be applied to Neon:"; cat "$SP/neon-migrate.log"; exit 1; }
  if [ -f "$STATE/tenant" ]; then TID=$(cat "$STATE/tenant"); else TID=$(python3 -c 'import uuid; print(uuid.uuid4())'); echo "$TID" > "$STATE/tenant"; fi
  $PGBIN/psql -q "postgresql://prism_prov:$NEON_PROV_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?$NEON_SSL" -c "INSERT INTO tenants(id,name) VALUES ('$TID','Dev Firm') ON CONFLICT (id) DO NOTHING"
  # A sync that was stopped part way still holds its claim for up to half an hour; release it.
  $PGBIN/psql -q "postgresql://prism_worker:$NEON_WORKER_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?$NEON_SSL" -c "BEGIN; SELECT set_config('app.tenant_id','$TID',true); UPDATE sync_queue SET claimed_until = NULL, run_after = now(); COMMIT;" >/dev/null
  echo "Using the Neon database (migrations are up to date)."
else
PGPORT=$(free_port)
[ -f "$DATA/PG_VERSION" ] || "$PGBIN/initdb" -D "$DATA" -U postgres --auth=trust -E UTF8 >/dev/null
# A kept database may still be running from an unclean stop; shut that down before starting.
[ "$KEEP" = 1 ] && "$PGBIN/pg_ctl" -D "$DATA" stop -m fast >/dev/null 2>&1 || true
"$PGBIN/pg_ctl" -D "$DATA" -o "-p $PGPORT -c listen_addresses=127.0.0.1 -k $SP" -w start >/dev/null
PSQL="$PGBIN/psql -h 127.0.0.1 -p $PGPORT -q -U postgres"
$PSQL -tAc "SELECT 1 FROM pg_database WHERE datname = 'dev'" | grep -q 1 || $PSQL -c "CREATE DATABASE dev"
# The meaning-search model is fetched once, here, so the server and the worker do not both try to
# download it at the same moment. If it cannot be fetched, search falls back to matching words.
$BIN download-model >/dev/null 2>&1 || echo "Note: the meaning-search model could not be downloaded; search will match words only."
ALLOW_INSECURE=1 DATABASE_URL="host=127.0.0.1 port=$PGPORT user=postgres dbname=dev" $BIN migrate >/dev/null 2>&1
$PSQL -d dev <<'SQL'
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'dev_api') THEN CREATE ROLE dev_api LOGIN PASSWORD 'pw' IN ROLE app_api; END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'dev_worker') THEN CREATE ROLE dev_worker LOGIN PASSWORD 'pw' IN ROLE app_worker; END IF;
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'dev_prov') THEN CREATE ROLE dev_prov LOGIN PASSWORD 'pw' IN ROLE app_provisioner; END IF;
END $$;
SQL
# The firm keeps the same identity across kept runs, so its data and saved access still match.
if [ "$KEEP" = 1 ] && [ -f "$STATE/tenant" ]; then TID=$(cat "$STATE/tenant"); else TID=$(python3 -c 'import uuid; print(uuid.uuid4())'); [ "$KEEP" = 1 ] && echo "$TID" > "$STATE/tenant"; fi
PGPASSWORD=pw $PGBIN/psql -h 127.0.0.1 -p $PGPORT -q -U dev_prov -d dev -c "INSERT INTO tenants(id,name) VALUES ('$TID','Dev Firm') ON CONFLICT (id) DO NOTHING"
# A sync that was stopped part way still holds its claim for up to half an hour; release it.
$PSQL -d dev -c "UPDATE sync_queue SET claimed_until = NULL, run_after = now()"
QUERY="$PSQL -d dev -At"

fi
# ---- stand-in for the app's own sign-in (the real one is Cognito) ----
# In keep mode the stand-in sign-in keeps its address, so a desktop app that is already open still
# holds a token the restarted server accepts.
if [ "$KEEP" = 1 ] && [ -f "$STATE/idp-port" ] && ! lsof -nP -iTCP:"$(cat "$STATE/idp-port")" -sTCP:LISTEN >/dev/null 2>&1; then
  IDPPORT=$(cat "$STATE/idp-port")
else
  IDPPORT=$(free_port); [ "$KEEP" = 1 ] && echo "$IDPPORT" > "$STATE/idp-port"
fi
python3 -I - "$TID" "$SP" "$IDPPORT" <<'PYEOF'
import sys,subprocess,base64,json,time,os
tid,sp,port=sys.argv[1:4]; fx='tests/fixtures/'
b=lambda x: base64.urlsafe_b64encode(x).rstrip(b'=').decode()
mod=subprocess.check_output(['openssl','rsa','-pubin','-in',fx+'test_rsa_public.pem','-noout','-modulus']).decode().split('=')[1].strip()
os.makedirs(sp+'/idp/pool/.well-known',exist_ok=True)
json.dump({"keys":[{"kty":"RSA","kid":"k1","alg":"RS256","use":"sig","n":b(bytes.fromhex(mod)),"e":"AQAB"}]},open(sp+'/idp/pool/.well-known/jwks.json','w'))
claims={"sub":"dev-admin","iss":f"http://127.0.0.1:{port}/pool","aud":"dev-client","exp":int(time.time())+8*3600,
        "token_use":"id","custom:tenant_id":tid,"cognito:groups":["firm_admin"],"email":"admin@dev.local","email_verified":True}
h=b(json.dumps({"alg":"RS256","typ":"JWT","kid":"k1"}).encode()); p=b(json.dumps(claims).encode())
sig=subprocess.run(['openssl','dgst','-sha256','-sign',fx+'test_rsa_private.pem'],input=(h+'.'+p).encode(),capture_output=True).stdout
open(sp+'/token','w').write(h+'.'+p+'.'+b(sig))
PYEOF
(cd "$SP/idp" && exec python3 -m http.server "$IDPPORT" --bind 127.0.0.1 >/dev/null 2>&1) & PIDS+=($!)
sleep 1

# ---- API and worker ----
export ALLOW_INSECURE=1 DEV_SECRETS_DIR="$SECRETS"
export OAUTH_STATE_KEY="$(openssl rand -base64 32)"
export OAUTH_REDIRECT_URI="http://localhost:$API_PORT/oauth/callback"
export MICROSOFT_CLIENT_ID MICROSOFT_CLIENT_SECRET GOOGLE_CLIENT_ID GOOGLE_CLIENT_SECRET
# Optional: Dropbox and DocuSign appear in the app once these are in backend/.env.local.
export DROPBOX_CLIENT_ID DROPBOX_CLIENT_SECRET DOCUSIGN_CLIENT_ID DOCUSIGN_CLIENT_SECRET DOCUSIGN_ENVIRONMENT QUICKBOOKS_CLIENT_ID QUICKBOOKS_CLIENT_SECRET QUICKBOOKS_ENVIRONMENT ZOOM_CLIENT_ID ZOOM_CLIENT_SECRET ZOOM_REDIRECT_URI
if [ "$NEON" = 1 ]; then
  API_URL_FOR_DB="$API_DB_URL"; WORKER_URL_FOR_DB="$WORKER_DB_URL"
else
  API_URL_FOR_DB="host=127.0.0.1 port=$PGPORT user=dev_api password=pw dbname=dev"; WORKER_URL_FOR_DB="host=127.0.0.1 port=$PGPORT user=dev_worker password=pw dbname=dev"
fi
BIND=127.0.0.1:$API_PORT DATABASE_URL="$API_URL_FOR_DB" \
  COGNITO_ISSUER="http://127.0.0.1:$IDPPORT/pool" COGNITO_CLIENT_ID=dev-client $BIN >"$SP/api.log" 2>&1 & PIDS+=($!)
DATABASE_URL="$WORKER_URL_FOR_DB" $BIN worker >"$SP/worker.log" 2>&1 & PIDS+=($!)
U=http://127.0.0.1:$API_PORT; TOKEN=$(cat "$SP/token")
# The API loads the meaning-search model before it listens, which takes a few seconds.
for _ in $(seq 1 60); do curl -s -m 2 -o /dev/null -f $U/readyz && break; sleep 1; done
if ! curl -s -m 5 -o /dev/null -f $U/readyz; then echo "The API did not start:"; tail -5 "$SP/api.log"; exit 1; fi

# Let the desktop app find this server: `npm run tauri:dev:cloud` reads this file.
( umask 077; printf 'PRIVATE_AI_CLOUD_API=%s\nPRIVATE_AI_CLOUD_DEV_TOKEN=%s\n' "$U" "$TOKEN" > .env.devcloud )

if [ "$KIND" = "" ]; then
  if [ "$SEED" = 0 ]; then
    echo "Server ready with the connectors set up in .env.local. Nothing is connected yet."
    echo "In another terminal, from the project folder:   npm run tauri:dev:cloud"
    echo "Then open Sources > Connectors in the app and press Connect."
    read -r -p "Press Enter here to stop the server. " _ || true
    exit 0
  fi
  # Sample mode: put three files straight into the firm's cloud index. The memo is readable only
  # by someone else, to show that permissions are respected.
  PGPASSWORD=pw $PGBIN/psql -h 127.0.0.1 -p $PGPORT -q -U dev_worker -d dev <<SQL
BEGIN; SELECT set_config('app.tenant_id','$TID',true) \gset
INSERT INTO connections(tenant_id,kind,secret_ref) VALUES (app_tenant(),'google_drive','sample');
CREATE TEMP TABLE seed(name text, body text, readers text[]);
INSERT INTO seed VALUES
 ('Cloud Engagement Letter.txt','The firm cloud copy of the engagement letter: fees are due within thirty days of the invoice. Late payments accrue interest at one percent a month.',ARRAY['user:dev-admin']),
 ('Intake Procedure.md','New client intake procedure: confirm the engagement letter is signed before any work begins, then open the client folder.',ARRAY['user:dev-admin']),
 ('Partner Memo.txt','Memo to the partners about the settlement strategy for the Globex matter.',ARRAY['user:someone-else']);
INSERT INTO documents(tenant_id,connection_id,external_id,name,readable_by)
  SELECT app_tenant(),(SELECT id FROM connections),name,name,readers FROM seed;
INSERT INTO chunks(tenant_id,document_id,ordinal,text)
  SELECT app_tenant(),d.id,0,s.body FROM documents d JOIN seed s ON s.name=d.name;
COMMIT;
SQL
  echo "Sample mode: 3 files in the cloud index (the partner memo is hidden from you on purpose)."
  echo "In another terminal, from the project folder:   npm run tauri:dev:cloud"
  echo "Or check the wire format:   cd src-tauri && set -a && . ../backend/.env.devcloud && set +a && cargo test live_server -- --ignored"
  read -r -p "Press Enter here to stop the server. " _ || true
  exit 0
fi

# ---- start the connection and check the provider accepts the app settings ----
RESPONSE=$(curl -s -m 10 -X POST $U/v1/connections -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d "{\"kind\":\"$KIND\"}")
URL=$(python3 -c 'import sys,json; print(json.loads(sys.argv[1])["authorization_url"])' "$RESPONSE")
curl -sL -m 30 -o "$SP/ms.html" -w '' "$URL" || true
ERRORS=$(grep -o -i 'AADSTS[0-9]\{4,6\}[^"<\\]\{0,200\}\|invalid_client\|Error 400: [a-z_]*\|Error 401: [a-z_]*' "$SP/ms.html" | head -3 || true)
if [ -n "$ERRORS" ]; then
  echo "$PROVIDER rejected the app settings (check the client ID in backend/.env.local):"; echo "$ERRORS"; exit 1
fi
echo "$PROVIDER accepted the client ID. (A redirect address mismatch only shows after you sign in.)"
[ "$CHECK_ONLY" = 1 ] && exit 0

echo "Opening the $PROVIDER sign-in in your browser. Approve access, then come back here."
open "$URL" 2>/dev/null || echo "Open this link yourself: $URL"

echo -n "Waiting for you to approve"
for _ in $(seq 1 120); do
  [ "$($QUERY -c "SELECT count(*) FROM connections")" != "0" ] && break
  echo -n "."; sleep 3
done
echo
if [ "$($QUERY -c "SELECT count(*) FROM connections")" = "0" ]; then
  echo "No connection arrived. API log:"; tail -5 "$SP/api.log"; exit 1
fi
echo "Connected. Syncing your files..."
for _ in $(seq 1 100); do
  [ -n "$($QUERY -c "SELECT last_sync_at FROM connections WHERE last_sync_at IS NOT NULL")" ] && break
  ERR=$($QUERY -c "SELECT coalesce(last_error,'') FROM connections")
  [ -n "$ERR" ] && { echo "Sync problem: $ERR"; tail -5 "$SP/worker.log"; break; }
  sleep 3
done
echo "Files stored: $($QUERY -c "SELECT count(*) FROM documents")  (readable text: $($QUERY -c "SELECT count(*) FROM documents WHERE status='indexed'"), name only: $($QUERY -c "SELECT count(*) FROM documents WHERE status='metadata_only'"))"
$QUERY -c "SELECT '  ' || name || '  [' || status || ']' FROM documents ORDER BY name LIMIT 15"

echo
while true; do
  read -r -p "Search your files (blank to quit): " Q || break
  [ -z "$Q" ] && break
  BODY=$(python3 -c 'import json,sys; print(json.dumps({"query": sys.argv[1]}))' "$Q")
  curl -s -m 15 -X POST $U/v1/search -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d "$BODY" |
    python3 -c '
import json,sys
r=json.load(sys.stdin).get("results",[])
if not r: print("  (nothing found)")
for x in r[:6]: print("  -", x["document_name"], "|", x["text"][:140].replace("\n"," "))'
done
