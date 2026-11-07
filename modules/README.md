# Modules

Local OpenTofu modules wired by the `envs/dev` and `envs/prod` roots.

- `bootstrap` — remote-state S3 bucket (versioning, SSE-S3, public-access-block,
  lifecycle).
- `bootstrap-iam` — per-account GitHub OIDC provider and the `gha-deploy` role.
- `data` — the single DynamoDB table (PK/SK, GSI1 owner queue, GSI2 collection
  membership, TTL, PITR) plus a throttle alarm.

`site`, `media`, `artifacts`, `config`, `api`, and `cdn` arrive in later
tickets.
