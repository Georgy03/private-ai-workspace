# Private AI Connector Workspace

Private AI Connector Workspace is a Tauri desktop application for private, cited search over folders the user explicitly approves. Indexing, retrieval, and citations remain on the device. AI answers come from Claude on Amazon Bedrock, and the app sends nothing unless the AWS account reports zero data retention.

## Current slice

- Encrypted SQLCipher workspace database; its random key lives in macOS Keychain or Windows Credential Manager.
- Local PDFs, DOCX, spreadsheets, CSV, TXT, and Markdown are parsed in a separate process, hashed, and indexed with FTS5 citations down to page, heading, sheet, or cell range.
- Cancellation keeps completed files. Rescans reconcile changes and deleted files. Unsupported, oversized, symlinked, and unreadable files are reported rather than silently indexed.
- Questions also search the firm's **cloud index** (Microsoft 365 and Google Drive content synced by the backend in `backend/`), when it is set up and the person is signed in. Local and cloud passages are merged on this device, and every cloud passage is labelled. Only the question text goes to the cloud service; local files are never uploaded to it. If the cloud is off, signed out or unreachable, the answer uses local files and says so. The privacy receipt records whether the question went to the cloud index.
- **Sources → Connectors** lists the connectors the firm's server offers (Google Drive, Microsoft 365), lets a firm administrator add one (the provider's sign-in opens in the browser), and shows each connected source's status, file count and last sync, with Sync now, Reconnect and Disconnect. Dropbox, Box, Clio and NetDocuments are shown as coming soon.
- Each answer sends the question plus the matching passages to Claude on Amazon Bedrock. Summary-style questions (for example "what is this case about") send the full text of the top-matching document. The rest of the index is never uploaded.
- Before each request the app reads the account's data retention mode for the Region and refuses to send unless it is `none`. There is no fallback provider.
- Search keeps working without AWS access.
- The UI shows folders, sources, citations, indexing status, and a privacy receipt saying what was sent for each answer.

## Setting up Amazon Bedrock

The app uses the standard AWS credential chain and Region (`aws configure`, `AWS_PROFILE`, `AWS_REGION`). The default model is `us.anthropic.claude-sonnet-5-5`; override it with `PRIVATE_AI_CONNECTOR_WORKSPACE_BEDROCK_MODEL`.

1. Open the model in the Bedrock console playground, complete the first-time use form if it appears, and send a test message. The first use starts the AWS Marketplace agreement, which can take a few minutes. If access is still denied afterwards, the account may need access from AWS Sales.
2. Set zero data retention for your Region. The setting is per Region and applies to the whole account:

```sh
curl -X PUT "https://bedrock.$AWS_REGION.amazonaws.com/data-retention" \
  --aws-sigv4 "aws:amz:$AWS_REGION:bedrock" \
  -u "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" \
  -H "content-type: application/json" \
  -d '{"mode":"none"}'
```

Zero data retention means AWS does not write requests or responses to durable storage. They are still decrypted on AWS infrastructure while being processed, and AWS may store and review apparent child sexual abuse material. See the Bedrock data retention and abuse detection documentation.

## Test mode: direct Claude API

For development without Bedrock, `npm run tauri:dev:test` sends requests to the Claude API using `claude-sonnet-5-5`. **This mode has no zero data retention and no retention check, so use test documents only, never client files.** The app shows a banner while it is on, and release builds refuse it and always use Bedrock.

The key is read from `ANTHROPIC_API_KEY`, or else from a private file in the workspace folder. Copy the key, then:

```sh
f="$HOME/Library/Application Support/PrivateAIConnectorWorkspace/anthropic-api-key"
pbpaste > "$f" && chmod 600 "$f"
```

Debug builds also keep the local index key in that folder (`database.key`) instead of the Keychain, because rebuilding an unsigned app makes macOS ask for the login password on every launch. Release builds always use the Keychain.

## Development

```sh
npm install
npm test
npm run build
npm run tauri:dev
```

Native checks:

```sh
cd src-tauri
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

## Important boundary

This phase supports local folders and Claude on Amazon Bedrock only. Microsoft 365, Google Drive, organization sign-in, cloud synchronization, admin policy distribution, and a knowledge graph are deliberately deferred. A compromised signed-in computer can still expose information available to that computer's user; encrypted storage does not eliminate endpoint risk.
