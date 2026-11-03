#!/usr/bin/env bash
# Starts the real server against a throwaway Postgres and a stand-in identity provider, then
# checks sign-in, firm-scoped search and the audit log with curl. Needs openssl and python3.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q
PGBIN="${PGBIN:-/Library/PostgreSQL/18/bin}"
[ -x "$PGBIN/initdb" ] || PGBIN="$(dirname "$(command -v initdb)")"
SP="$(mktemp -d)"
cleanup() {
  [ -n "${SRV:-}" ] && kill "$SRV" 2>/dev/null || true
  [ -n "${IDP:-}" ] && kill "$IDP" 2>/dev/null || true
  "$PGBIN/pg_ctl" -D "$SP/data" stop -m immediate >/dev/null 2>&1 || true
  rm -rf "$SP"
}
trap cleanup EXIT
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

PGPORT=$(free_port)
"$PGBIN/initdb" -D "$SP/data" -U postgres --auth=trust -E UTF8 >/dev/null
"$PGBIN/pg_ctl" -D "$SP/data" -o "-p $PGPORT -c listen_addresses=127.0.0.1 -k $SP" -w start >/dev/null
PSQL="$PGBIN/psql -h 127.0.0.1 -p $PGPORT -q"
$PSQL -U postgres -c "CREATE DATABASE smoke"
BIN=target/debug/workspace-cloud
ADMIN="host=127.0.0.1 port=$PGPORT user=postgres dbname=smoke"
clean() { sed 's/\x1b\[[0-9;]*m//g' | tail -1 | cut -c28-120; }

echo "migrate (first run):  $(ALLOW_INSECURE=1 DATABASE_URL="$ADMIN" $BIN migrate 2>&1 | clean)"
echo "migrate (second run): $(ALLOW_INSECURE=1 DATABASE_URL="$ADMIN" $BIN migrate 2>&1 | clean)"
echo "migrate without TLS:  $(DATABASE_URL="$ADMIN" $BIN migrate 2>&1 | clean)"

$PSQL -U postgres -d smoke <<'SQL'
CREATE ROLE smoke_api LOGIN PASSWORD 'pw' IN ROLE app_api;
CREATE ROLE smoke_prov LOGIN PASSWORD 'pw' IN ROLE app_provisioner;
CREATE ROLE smoke_worker LOGIN PASSWORD 'pw' IN ROLE app_worker;
SQL
TID=$(python3 -c 'import uuid; print(uuid.uuid4())')
PGPASSWORD=pw $PSQL -U smoke_prov -d smoke -c "INSERT INTO tenants(id,name) VALUES ('$TID','Smoke Firm')"
PGPASSWORD=pw $PSQL -U smoke_worker -d smoke <<SQL
BEGIN; SELECT set_config('app.tenant_id','$TID',true);
INSERT INTO connections(tenant_id,kind,secret_ref) VALUES (app_tenant(),'google_drive','s/1');
INSERT INTO documents(tenant_id,connection_id,external_id,name,readable_by) SELECT app_tenant(),id,'x','Retainer.txt',ARRAY['user:alice'] FROM connections;
INSERT INTO chunks(tenant_id,document_id,ordinal,text) SELECT app_tenant(),id,0,'The retainer is due within thirty days.' FROM documents;
COMMIT;
SQL

IDPPORT=$(free_port); APIPORT=$(free_port)
python3 -I - "$TID" "$SP" "$IDPPORT" <<'PYEOF'
import sys,subprocess,base64,json,time,os
tid,sp,port=sys.argv[1:4]; fx='tests/fixtures/'
b=lambda x: base64.urlsafe_b64encode(x).rstrip(b'=').decode()
mod=subprocess.check_output(['openssl','rsa','-pubin','-in',fx+'test_rsa_public.pem','-noout','-modulus']).decode().split('=')[1].strip()
os.makedirs(sp+'/idp/pool/.well-known',exist_ok=True)
json.dump({"keys":[{"kty":"RSA","kid":"k1","alg":"RS256","use":"sig","n":b(bytes.fromhex(mod)),"e":"AQAB"}]},open(sp+'/idp/pool/.well-known/jwks.json','w'))
def make(extra):
    claims={"sub":"alice","iss":f"http://127.0.0.1:{port}/pool","aud":"smoke-client","exp":int(time.time())+600,"token_use":"id","custom:tenant_id":tid}; claims.update(extra)
    h=b(json.dumps({"alg":"RS256","typ":"JWT","kid":"k1"}).encode()); p=b(json.dumps(claims).encode())
    sig=subprocess.run(['openssl','dgst','-sha256','-sign',fx+'test_rsa_private.pem'],input=(h+'.'+p).encode(),capture_output=True).stdout
    return h+'.'+p+'.'+b(sig)
open(sp+'/token','w').write(make({})); open(sp+'/token_bob','w').write(make({"sub":"bob"}))
PYEOF
(cd "$SP/idp" && exec python3 -m http.server "$IDPPORT" --bind 127.0.0.1 >/dev/null 2>&1) & IDP=$!
sleep 1
ALLOW_INSECURE=1 BIND=127.0.0.1:$APIPORT DATABASE_URL="host=127.0.0.1 port=$PGPORT user=smoke_api password=pw dbname=smoke" \
  COGNITO_ISSUER="http://127.0.0.1:$IDPPORT/pool" COGNITO_CLIENT_ID=smoke-client $BIN >"$SP/server.log" 2>&1 & SRV=$!
sleep 2
U=http://127.0.0.1:$APIPORT
ask() { curl -s -m 5 -w ' [%{http_code}]' -X POST $U/v1/search -H "authorization: Bearer $1" -H 'content-type: application/json' -d '{"query":"when is the retainer due"}'; }
echo "readyz:              $(curl -s -m 5 -o /dev/null -w '%{http_code}' $U/readyz)"
echo "no token:            $(curl -s -m 5 -w ' [%{http_code}]' -X POST $U/v1/search -H 'content-type: application/json' -d '{"query":"x"}')"
echo "alice (allowed):     $(ask "$(cat $SP/token)" | cut -c1-240)"
echo "bob (not allowed):   $(ask "$(cat $SP/token_bob)")"
echo "audit rows:          $($PSQL -U postgres -d smoke -Atc "SELECT actor||' '||action||' '||detail FROM audit_events ORDER BY id" | tr '\n' ';')"
