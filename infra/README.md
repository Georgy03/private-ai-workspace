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
| `auth.tf` | Cognito user pool with required MFA, admin-only user creation, and a `custom:tenant_id` attribute that users cannot change |
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

## Not included yet

The API and worker containers (ECS, load balancer, container registry), the Bedrock credential
broker for the desktop app, Microsoft and Google federation in Cognito, VPC flow logs, a web
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
