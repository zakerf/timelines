# Modules

Local OpenTofu modules wired by the `envs/dev` and `envs/prod` roots.

- `bootstrap` — remote-state S3 bucket (versioning, SSE-S3, public-access-block,
  lifecycle).
- `bootstrap-iam` — per-account GitHub OIDC provider and the `gha-deploy` role.

`site`, `media`, `artifacts`, `data`, `config`, `api`, and `cdn` arrive in later
tickets.
