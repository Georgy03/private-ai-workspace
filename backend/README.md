# workspace-cloud

The cloud side of the multi-firm workspace: sign-in verification, per-firm database isolation and
the search API. See `../docs/architecture.md` for the design.

## Run the checks

```sh
scripts/test.sh     # 91 tests against a throwaway local Postgres
scripts/smoke.sh    # starts the real server and calls it with curl
scripts/dev-connect.sh microsoft|google   # try a connector with your own account (see the script header)
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

## Endpoints

| Route | Auth | Purpose |
|---|---|---|
| `GET /healthz` | none | Process is up (for the load balancer) |
| `GET /readyz` | none | The database answers |
| `GET /v1/me` | ID token | Firm, user and groups from the token |
| `POST /v1/search` | ID token | `{"query": "...", "limit": 20}` returns passages from files the user may read in the source system |
| `GET /v1/connectors` | ID token | The connectors this server offers and whether each is set up |
| `GET /v1/connections` | ID token | The firm's connected sources, their status and file counts |
| `POST /v1/connections` | firm admin | `{"kind": "microsoft365" \| "google_drive", "drive_id"?, "domains"?, "reconnect"?}` returns the provider sign-in link. `reconnect` is the ID of a connection that needs reconnecting, which is revived instead of duplicated |
| `GET /oauth/callback` | signed state | Where the provider sends the administrator back; stores the connection |
| `DELETE /v1/connections/{id}` | firm admin | Removes the connection, its files and its saved token |
| `POST /v1/connections/{id}/sync` | firm admin | Sync now |

The firm comes only from the token's `custom:tenant_id`. Searches are written to the audit table
as counts only; the question text is never stored.

## Running the server

```sh
workspace-cloud migrate   # applies migrations; run as the deployment database user
workspace-cloud           # serves the API
workspace-cloud worker    # keeps connected sources up to date; run as the app_worker login
```

| Variable | Meaning |
|---|---|
| `DATABASE_URL` | Connection string for the `app_api` login (for `migrate`, the deployment user) |
| `DATABASE_CA_FILE` | CA bundle for the database; required. For RDS use AWS's `global-bundle.pem` |
| `COGNITO_ISSUER` | Token issuer URL, `https://cognito-idp.<region>.amazonaws.com/<pool id>` |
| `COGNITO_CLIENT_ID` | The desktop app client ID (the token audience) |
| `BIND` | Address to listen on. Defaults to `127.0.0.1:8080` |
| `ALLOW_INSECURE` | `1` skips database TLS, allows http URLs and uses an in-memory secret store. Local development only |
| `OAUTH_STATE_KEY` | Base64, 32+ bytes. Signs the connection round trip. Without it connectors are switched off |
| `OAUTH_REDIRECT_URI` | The callback URL registered with Microsoft and Google |
| `MICROSOFT_CLIENT_ID`, `MICROSOFT_CLIENT_SECRET` | Azure app registration (multi-tenant) |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | Google Cloud OAuth client (web application) |

## Trying it on a developer machine

`scripts/dev-connect.sh ui --keep` starts the whole server (database, API, worker) locally with Google and
Microsoft available, and nothing connected: add sources from the app's Sources > Connectors tab, then run
`npm run tauri:dev:cloud:test` (direct Claude API, test documents only) or `npm run tauri:dev:cloud`.

Without `--keep`, everything is deleted when the script stops. With it, the database, the saved
Google/Microsoft access and the firm identity stay in `backend/.dev-state`, so a restart does not lose a sync.
That folder holds a **plain-text copy of your synced files**; it is ignored by git, and
`scripts/dev-connect.sh forget` deletes it.

## How connectors work

An administrator approves read-only access at the provider; the server keeps the refresh token in
the secret store, never in the database. The worker claims due connections from a queue, lists
changes with the provider's change feed (Microsoft Graph delta, Google Drive changes), stores each
file's text and who may read it, and saves its place after every page so a crash repeats a little
work and skips nothing. Every few hours it lists everything again, which re-reads permissions and
removes files that disappeared.

Work and school accounts and personal Microsoft accounts both work: sign-in uses Microsoft's
common endpoint and asks only for permissions personal accounts can grant. The person who
connected a source can always read everything it returns, so a solo user can search their own
files even when the provider lists no sharing entries for them.

Who may read a file is translated into `email:`, `domain:` and `group:` identities and matched
against the signed-in person's verified email. A file whose permissions are unknown is hidden from
everyone.

## Reading PDF and Office files

The worker downloads PDFs and Word, Excel and PowerPoint files (up to 50 MB) and reads their text with
`workspace-extract`, a separate program started once per file. Passages carry their place in the file
("Page 3", "Slide 2", "Sheet: Fees"), which appears in search results. Scanned pages have no text and
come back empty, so the file stays findable by name.

Files from clients are untrusted, so the reader is restricted: it starts with an empty environment and an
empty working folder, lowers its own limits (CPU time, no file writes, no crash dumps, open files, and
memory on Linux), is stopped after 60 seconds, and its output is capped. Archives are read with byte
counts rather than the sizes the file claims, and XML entities are never expanded. A reader that crashes
or times out is retried later; a damaged file is not.

**Before deploying:** these limits do not cut the reader off from the network or from files its user
can read. Run it in a container with no network access and a read-only file system, as a different user
from the worker, with no credentials mounted. `EXTRACTOR_PATH` points the worker at the program.

When the reader improves, bump `PARSER_VERSION` in `src/extract.rs`; files read by an older version are read
again on the next sync.

## Known limits

- **Only tested against stand-ins** for Microsoft Graph and Google Drive. Real services will differ in
  details (delta behaviour for permission changes, rate limits, which files show their permissions).
- **No production secret store yet.** The AWS Secrets Manager store, reached through the per-firm
  `tenant_access` role, is not written. Outside development the server refuses to save tokens.
- **Scanned PDFs, photos and old .doc/.ppt files are found by name only.** The file reader handles PDFs
  that have a text layer, plus .docx, .pptx and .xlsx/.xls. Scans need OCR, which is not built.
- **Group sharing does not grant access yet.** A file shared only with a group (an Entra group or a
  Google group) stays hidden unless the identity provider reports that group in the token's
  `cognito:groups`. SharePoint groups grant nobody. Public and anyone-with-link sharing is ignored.
- **Google reads what the connecting account can see.** Drive shows permissions only to people who can
  share a file, so such files are visible only to the person who connected the source. Whole-organisation Google access needs a service account
  with domain-wide delegation, which is not built.
- **Paths are file names only**, and a revoked share can stay visible for up to six hours.

## Not done yet

Per-user rate limits, database login through IAM instead of a password, a container image, the
production secret store, embeddings, and writing the firm ID into Cognito users at sign-up.
