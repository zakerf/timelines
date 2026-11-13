# Modules

Local OpenTofu modules wired by the `envs/dev` and `envs/prod` roots.

- `bootstrap` — remote-state S3 bucket (versioning, SSE-S3, public-access-block,
  lifecycle).
- `bootstrap-iam` — per-account GitHub OIDC provider and the `gha-deploy` role.
- `data` — the single DynamoDB table (PK/SK, GSI1 owner queue, GSI2 collection
  membership, TTL, PITR) plus a throttle alarm.
- `site` — private, versioned rendered-pages S3 bucket with an OAC bucket
  policy (`AWS:SourceAccount`, no cycle).
- `media` — private, unversioned cover-image S3 bucket with the same OAC
  policy; keys are immutable and replacements are deleted.
- `cdn` — CloudFront distribution, OAC, extensionless→`.html` rewrite function,
  `403`/`404` → `/404.html`, the `/media/*` behavior (`CachingOptimized` +
  `SecurityHeadersPolicy`, no rewrite), the `/api/*` behavior
  (`CachingDisabled` + `AllViewerExceptHostHeader`), and the distribution-id
  SSM parameter.
- `config` — the OIDC seam's non-secret SSM parameter (four URLs + client id +
  `token_endpoint_auth_method=none`) and the out-of-band IP-salt name.
- `api` — Lambda (`provided.al2023`/arm64/256 MB/10 s, `live` alias), inline
  IAM policy, log group, HTTP API `$default` route/stage, and alarms.

`artifacts` arrives in a later ticket.
