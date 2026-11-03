#!/usr/bin/env bash
set -euo pipefail

# Signs a catalog payload with the release key kept in macOS Keychain. The private key never
# enters the repository or command output. Usage: scripts/sign-model-catalog.sh payload.json catalog.json
payload=${1:?payload JSON path required}
output=${2:?output catalog path required}
key_file=$(mktemp)
trap 'rm -f "$key_file"' EXIT
security find-generic-password -a "model-catalog-signing-key" -s "ai.private.connector.workspace.release" -w \
  | base64 -D > "$key_file"
signature=$(openssl pkeyutl -sign -rawin -inkey "$key_file" -keyform DER -in "$payload" | base64)
jq -Rs --arg signature "$signature" '{payload: ., signature: $signature}' "$payload" > "$output"
