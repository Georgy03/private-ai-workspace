# Infrastructure (OpenTofu)

AWS resources for the multi-firm cloud service described in `../docs/architecture.md`.

**Status: written and checked with `tofu validate`. Never planned or deployed.** `validate` only
checks syntax and provider schemas. It does not prove the permission rules behave as intended, and
the first `tofu plan` in a real account will likely need small fixes.

## What it creates

| File | Resources |
|---|---|
| `network.tf` | VPC, private subnets, one NAT gateway, locked default security group, private endpoints for S3, KMS and Secrets Manager |
| `kms.tf` | A platform encryption key and a separate audit-log key, both with rotation |
| `storage.tf` | The shared data bucket (private, versioned, TLS only, KMS-encrypted uploads required) and a write-once audit bucket |
| `database.tf` | Postgres on RDS: private, encrypted, forced TLS, 14-day backups, deletion protection, admin password held by AWS |
| `auth.tf` | Cognito user pool with required MFA, admin-only user creation, and a `custom:tenant_id` attribute that users cannot change |
| `iam.tf` | Service roles and the `tenant_access` role that limits storage, keys and secrets to one firm per session |
| `audit.tf` | CloudTrail with file-level data events, GuardDuty, alerts for medium-or-higher findings and root sign-in, a monthly budget alarm |

## How firms are kept apart on AWS

The services never touch firm data with their own roles. They assume `tenant_access` with a
session tag `tenant=<firm id>`, and every permission is written against that tag: files under
`tenants/<firm id>/`, KMS keys tagged `tenant=<firm id>`, secrets under `tenants/<firm id>/`.
Each firm's KMS key is created at runtime by the provisioner, not by this code, so adding a firm
never changes the infrastructure. The database is separated by row-level security in
`../backend`.

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
