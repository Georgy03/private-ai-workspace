# Infrastructure (OpenTofu)

AWS resources for the multi-firm cloud service described in `../docs/architecture.md`.

**Status: written and checked with `tofu validate`. Never planned or deployed.** `validate` only
checks syntax and provider schemas. It does not prove the permission rules behave as intended, and
the first `tofu plan` in a real account will likely need small fixes.

## What it creates

| File | Resources |
|---|---|
| `network.tf` | VPC, private subnets, one NAT gateway, locked default security group, private endpoints for S3 and KMS |
| `kms.tf` | A platform encryption key and a separate audit-log key, both with rotation. Each firm's own key is made later, at runtime |
| `storage.tf` | The shared data bucket (private, versioned, TLS only, KMS-encrypted uploads required) and a write-once audit bucket |
| `database.tf` | Postgres on RDS: private, encrypted, forced TLS, 14-day backups, deletion protection, admin password held by AWS |
| `auth.tf` | Cognito user pool with required MFA, admin-only user creation, a `custom:tenant_id` attribute that users cannot change, and the `firm_admin` group |
| `signin.tf`, `presignup/` | Sign in with Google and Microsoft for invited people only: the providers, and the pre sign-up function (Python, with tests) that links each person to their invitation |
| `iam.tf` | Service roles; the `tenant_access` role that limits a firm's files and keys to one firm per session; the permissions to use firm keys for connector tokens; and the provisioner that makes them |
| `audit.tf` | CloudTrail with file-level data events, GuardDuty, alerts for medium-or-higher findings and root sign-in, a monthly budget alarm |

## How firms are kept apart on AWS

The services never touch a firm's files with their own roles. They assume `tenant_access` with a
session tag `tenant=<firm id>`, and every permission is written against that tag: files under
`tenants/<firm id>/` and KMS keys tagged `tenant=<firm id>`.

Connector tokens are different: they are encrypted rows in the database, each under its firm's own KMS key
(see `../backend/README.md`, "Connector tokens in production"). The API and worker roles may use only keys
tagged `app=prism-workspace`, and only in calls whose encryption context is exactly the firm and the secret
name, so KMS refuses to open a token for any other firm or name. The provisioner makes each firm's key (tagged,
named `alias/prism-tenant-<firm id>`, rotating) at runtime with `workspace-cloud provision-firm`, so adding a
firm never changes the infrastructure. It cannot use a key to read anything. The database is separated by
row-level security in `../backend`.

## The database

`database.tf` creates the database on RDS, the all-AWS option. The project's current plan is Neon instead
(see `../backend/README.md`), in which case `database.tf` is not needed and should not be applied as it is.

## Setting up the AWS account (only what the first deployment needs)

You need the AWS CLI and OpenTofu, signed in as an administrator of the account (`aws sts get-caller-identity`
shows the account), and region `us-east-2`. This creates **13 small resources and no running servers**: the sign-in
user pool, its app, the administrators group, the Google provider, the pre sign-up function with its
role and log, and one limited IAM user for the Railway services. It does not create the network (and its NAT
gateway), the RDS database, the buckets or the audit tools that the rest of this folder describes. Cost: Cognito and
Lambda are free at this size, each firm's key is about $1 a month, and logs are pennies. Check current pricing.

1. **Protect the account** (console, 5 minutes). Turn on multi-factor for the root user and delete any root access
   keys. Under Billing, Budgets, make a monthly cost budget (for example $50) with an email alert.
2. **Keep the state safe.** The state file will hold the Google secret, so use an encrypted, versioned
   bucket (the name must be unique; this one uses your account number):
   ```sh
   BUCKET=prism-tofu-state-$(aws sts get-caller-identity --query Account --output text)
   aws s3api create-bucket --bucket $BUCKET --region us-east-2 --create-bucket-configuration LocationConstraint=us-east-2
   aws s3api put-public-access-block --bucket $BUCKET --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
   aws s3api put-bucket-versioning --bucket $BUCKET --versioning-configuration Status=Enabled
   aws s3api put-bucket-encryption --bucket $BUCKET --server-side-encryption-configuration '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"aws:kms"}}]}'
   cat > backend.tf <<EOF
   terraform {
     backend "s3" {
       bucket       = "$BUCKET"
       key          = "core/terraform.tfstate"
       region       = "us-east-2"
       encrypt      = true
       use_lockfile = true
     }
   }
   EOF
   ```
3. **Settings.** Copy `terraform.tfvars.example` to `terraform.tfvars` and set `alert_email` (the only required
   setting). Leave the Google settings empty for now: sign-in with a temporary password works without them.
4. **Create it.** `tofu init`, then `./apply-core.sh`. Read the plan and answer `yes`. It says a few resources fewer until
   Google is switched on (one more is that provider). Then run **`./finish-setup.sh`**, which does
   steps 5 to 8 below for you, in order, and can be run again safely. The steps are written out in case you prefer to do
   them by hand.
5. **Read what it made.** `tofu output`, and note `cognito_issuer`, `cognito_desktop_client_id`,
   `cognito_hosted_ui_url` and `services_iam_user`. Put the first two into `backend/.env.railway` as `COGNITO_ISSUER`
   and `COGNITO_CLIENT_ID` (the desktop app's `PRIVATE_AI_COGNITO_DOMAIN` is the third).
6. **Make the services' access key** and append it to your private file without printing it (the secret is shown
   only once, so this writes it straight in):
   ```sh
   aws iam create-access-key --user-name "$(tofu output -raw services_iam_user)" \
     --query 'AccessKey.[AccessKeyId,SecretAccessKey]' --output text \
     | awk '{printf "AWS_ACCESS_KEY_ID=%s\nAWS_SECRET_ACCESS_KEY=%s\n",$1,$2}' >> ../backend/.env.railway
   ```
7. **Make the first firm and its key**, with your own administrator sign-in and the provisioner's database login:
   ```sh
   cd ../backend && set -a && . ./.env.neon && set +a
   DATABASE_URL="postgresql://prism_prov:$NEON_PROV_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?sslmode=require" \
   DATABASE_CA_FILE=/etc/ssl/cert.pem AWS_REGION=us-east-2 \
   cargo run --bin workspace-cloud -- provision-firm "Your firm's name"
   ```
   It prints `tenant_id` (the firm's ID) and `kms_key_arn`.
8. **Test the real thing.** This is the first contact with real KMS: it stores, reads back and deletes a test secret
   through the production path, using the services' key and the API's database login.
   ```sh
   set -a && . ./.env.neon && . ./.env.railway && set +a
   DATABASE_URL="postgresql://prism_api:$NEON_API_PASSWORD@$NEON_POOLED_HOST/$NEON_DB?sslmode=require" \
   DATABASE_CA_FILE=/etc/ssl/cert.pem \
   cargo run --bin workspace-cloud -- check-secrets <tenant_id from step 7>
   ```
   It should print `ok`. An access-denied message names what is missing (usually a key tag or the encryption context
   in the policy): fix that before going further.
9. **Invite yourself** (see below) and **put the settings on Railway** with `backend/scripts/railway-vars.sh --deploy`.
   Railway also needs `OAUTH_REDIRECT_URI` (the API's Railway domain plus `/oauth/callback`) and at least one
   connector's client ID and secret in `backend/.env.railway`.

Not part of this: Claude through Bedrock for the desktop app (turn on model access in the Bedrock console, in a
region and account set to zero data retention) is a separate step, and is not needed to run the cloud service.

## Signing in with Google and Microsoft

People sign in with the Google or Microsoft account they already use, and an administrator decides who gets in.

**How it works.** An administrator first creates each person in the pool (their email, verified, and their firm in
`custom:tenant_id`). When that person signs in with Google or Microsoft for the first time, the pre sign-up function
(`presignup/handler.py`) finds the invitation and links the outside identity to it, so the tokens carry the invited
account's firm and groups. Anyone not invited is refused, so the pool never fills with accounts that have no firm.
Cognito's own multi-factor setting does not apply to these sign-ins: each firm's Google or Microsoft account policy
does, so ask every firm to require multi-factor there.

**Why Microsoft is set up once per firm.** Cognito requires a token's issuer to match the provider's issuer
exactly, and a Microsoft work account's issuer belongs to the firm's own Microsoft tenant, so Microsoft's shared
"common" address does not work (this is a widely reported Cognito limitation; I found no official fix, so confirm
it against AWS's current documentation). Each firm gets its own provider, tied to its tenant and its firm ID, and the
function lets only people invited to that firm use it. Google is one provider for every firm, and only an email
that Google itself has verified is accepted.

**It is automatic.** The providers are not in Terraform. `backend/scripts/firm.sh provision "Name" --microsoft-tenant <id>`
(or `add-microsoft` later, `remove-microsoft` to end it) makes the firm's provider through AWS, records which firm it
belongs to in the parameter store under `/prism/signin/microsoft/` (the pre sign-up function reads that), and allows it
on the app, undoing the earlier steps if a later one fails. Adding a firm changes no infrastructure code.

**One-time setup (yours, in two consoles)**

1. Run `./apply-core.sh`, then read the two outputs: `cognito_hosted_ui_url` (the app's `PRIVATE_AI_COGNITO_DOMAIN`)
   and `identity_provider_redirect_uri`.
2. *Google.* In Google Cloud, create an OAuth client of type Web application. Add `identity_provider_redirect_uri` as
   an authorized redirect URI. On the consent screen use only the `openid`, `email` and `profile` scopes (these need no
   Google review) and publish the app, or only listed test users can sign in. Keep this app separate from the Drive
   connector's: it must not be able to read files. Put its client ID in `terraform.tfvars` and its secret in
   `TF_VAR_google_client_secret`, then apply (the secret ends up in the state file, so keep the state encrypted). Finally run
   `backend/scripts/firm.sh enable-google` once: OpenTofu leaves the app's list of providers alone, so this is what allows Google on the app.
3. *Microsoft.* In the Microsoft Entra admin center, register one application for "Accounts in any organizational
   directory (multitenant)". Add `identity_provider_redirect_uri` as a Web redirect URI. Create a client secret (it
   expires, at most after 24 months: put the date in a calendar). Under Token configuration add the optional claim
   `email` to the ID token. Delegated Microsoft Graph permissions `openid`, `email` and `profile` are enough. Consider
   publisher verification, or some firms' Microsoft settings will block people from consenting. Save its ID and secret
   in `backend/.env.signin` (git-ignored) as `MICROSOFT_SIGNIN_CLIENT_ID` and `MICROSOFT_SIGNIN_CLIENT_SECRET`.
   The secret is sent only to Cognito, never kept in the parameter store.

**For each firm that uses Microsoft**

1. Ask the firm's Microsoft administrator to approve the app for their tenant:
   `https://login.microsoftonline.com/<their tenant id>/adminconsent?client_id=<MICROSOFT_SIGNIN_CLIENT_ID>`
2. Run `backend/scripts/firm.sh add-microsoft <firm id> <their Microsoft tenant id>`.

The desktop app's sign-in screen asks for a work email. The server looks up the invitation for that address
(`POST /v1/sign-in/route`), finds the firm's Microsoft provider and tells the app which one to open, so nobody
needs to know a provider name or claim an email domain. An address that was never invited, or whose firm has no
Microsoft sign-in, gets the same empty answer. The lookup is limited to 60 requests a minute in total.

**Open sign-up (a person starts their own firm).** On the sign-in screen, "Create my firm" signs the person in with
Google and then calls `POST /v1/onboarding/firm`. The server makes the firm and its own encryption key, attaches the
person to it in the user pool and makes them its administrator; the app then refreshes their token. The pre sign-up
function lets a Google address that Google verified, and that nobody has used, in with an account that has no firm:
that account can do nothing except ask for a firm. A person gets one firm, an invited person cannot make another, and
the whole server allows 20 new firms a minute. It is **off by default**. To switch it on:

1. Set `self_serve_firms = true` in `terraform.tfvars` and run `./apply-core.sh` (this lets the API's access key make
   firm keys and attach a person to a firm; read the comment in `service_access.tf` for what that costs).
2. Give the API service on Railway `ONBOARDING_DATABASE_URL`: the `prism_prov` login (the only database login that may
   create a firm), the same way `DATABASE_URL` is set.

The first version runs inside the API for simplicity. Once there are paying customers, move it to a small service of
its own with its own access key, so a taken-over API could not attach anyone to a firm. A person who signs up with
Google and later is invited to another firm by that same address will not be linked automatically (their Google
account already exists); an administrator would need to link them by hand. Personal Microsoft accounts
(`@outlook.com`, `@hotmail.com`, `@live.com` and a few other addresses Microsoft issues) can start a firm too: see
the next section.

**Clearing out abandoned sign-ups.** Open sign-up leaves an account with no firm behind for anyone who signs in and
never finishes. `backend/scripts/firm.sh prune-newcomers [--days 30]` lists those older than the given days (at least
1); add `--delete` to delete them. It only ever considers accounts that signed in through Google or Microsoft and
belong to no firm, so an invited person, an administrator or anyone attached to a firm is never touched, and it
checks each account again just before deleting it. It needs your own AWS sign-in (the `ClearAbandonedSignUps` permission
in `iam.tf`). A deleted person who comes back simply signs up again.

**Personal Microsoft accounts.** They all sign in through one fixed Microsoft address, so one shared provider serves
everyone: run `backend/scripts/firm.sh add-microsoft-personal` once (it uses the same Microsoft sign-in app, whose
account type must allow personal accounts: "Any Entra ID Tenant + Personal Microsoft accounts"). Microsoft does not
say whether a personal account's email is verified, so the sign-up function accepts this provider only for addresses
Microsoft itself issues, where the address is the account, and refuses any other address. In the app, "Continue with
Microsoft" tries the person's own firm's Microsoft sign-in first and falls back to this one for such an address.
`remove-microsoft-personal` turns it off.

**To invite a person** (use their work email; the firm ID comes from `provision-firm`):

```sh
aws cognito-idp admin-create-user --user-pool-id <pool id> --username sam@firm.example \
  --user-attributes Name=email,Value=sam@firm.example Name=email_verified,Value=true Name=custom:tenant_id,Value=<firm id> \
  --message-action SUPPRESS
aws cognito-idp admin-add-user-to-group --user-pool-id <pool id> --username sam@firm.example --group-name firm_admin   # administrators only
```

`--message-action SUPPRESS` sends no email, because they sign in with Google or Microsoft. For a firm with neither,
leave it off: Cognito emails a temporary password and they set up an authenticator app. Their address must match the
one their Google or Microsoft account uses.

**What to expect.** The sign-in page Cognito shows lists Google and each firm's `ms-<name>`; the desktop app does not
choose for the person yet. After linking, a person's first sign-in may show an "already found an entry for username"
message: signing in a second time works (this is how Cognito behaves when linking happens in a pre sign-up function;
if it does not clear on the second try, check the function's log in CloudWatch). Nothing here has been applied to
an AWS account or tried against real Google or Microsoft sign-in: the function's logic is tested, and the Terraform
validates, but the first real sign-in will probably need small fixes.

Test the function: `cd infra/presignup && python3 test_handler.py`.

## Not included yet

The API and worker containers (ECS, load balancer, container registry), the Bedrock credential
broker for the desktop app, VPC flow logs, a web
application firewall, and the pgvector extension setup. Each is a later step.

## Check without deploying

```sh
cd infra
tofu fmt -check -recursive
tofu init -backend=false
tofu validate
```

## Before a real deploy

1. Create a **new, separate AWS account** for this (not the one that holds Bedrock) and sign in
   with short-lived credentials, not long-term keys.
2. Create an encrypted, versioned S3 bucket for state with locking, and add it as the backend.
3. Copy `terraform.tfvars.example` to `terraform.tfvars` and set `alert_email`.
4. Run `tofu plan`, read it, and compare the cost with the estimate in the architecture plan.
   Rough running costs before any firm: NAT gateway, two interface endpoints in two zones, the
   database, GuardDuty and CloudTrail data events.
5. Confirm the alert email subscription when AWS sends it, or no alert will arrive.
