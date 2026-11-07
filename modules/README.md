# Modules

Local OpenTofu modules wired by the `envs/dev` and `envs/prod` roots.

- `bootstrap` — remote-state S3 bucket (versioning, SSE-S3, public-access-block,
  lifecycle).
- `bootstrap-iam` — per-account GitHub OIDC provider and the `gha-deploy` role.
- `data` — the single DynamoDB table (PK/SK, GSI1 owner queue, GSI2 collection
  membership, TTL, PITR) plus a throttle alarm.
- `site` — private, versioned rendered-pages S3 bucket with an OAC bucket
  policy (`AWS:SourceAccount`, no cycle).
- `cdn` — CloudFront distribution, OAC, extensionless→`.html` rewrite function,
  `403`/`404` → `/404.html`, and the distribution-id SSM parameter.

`media`, `artifacts`, `config`, and `api` arrive in later tickets.
