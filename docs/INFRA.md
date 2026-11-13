# Infrastructure

OpenTofu lives in `modules/` (local modules) and `envs/dev` + `envs/prod`
(roots). This ticket wires the account-level bootstrap; every other module
arrives in later tickets.

## Local-first, validated not applied

There is no AWS account wired up yet, so infrastructure changes are validated,
never applied:

```sh
tofu fmt -check -recursive
tofu -chdir=envs/dev init -backend=false
tofu -chdir=envs/dev validate
# same two commands for envs/prod
```

`tofu validate` checks configuration and provider schemas without contacting
AWS. No credentials are needed or present.

## Real-AWS-only pieces

These resources cannot be meaningfully exercised locally and are only created
against a real account:

- the **remote-state S3 bucket** (`modules/bootstrap`) — versioning, SSE-S3
  (AES256), public-access-block, abort-incomplete-multipart-uploads and
  90-day noncurrent-version expiry;
- the per-account **GitHub OIDC provider** (`token.actions.githubusercontent.com`);
- the **`gha-deploy` role** with `PowerUserAccess` plus the inline `app-iam`
  policy (role-scoped IAM writes and `iam:PassRole` to Lambda);
- account-id lookups (`aws_caller_identity`), which only resolve at plan/apply
  time;
- the **`site` S3 bucket** (`modules/site`) — private, versioned, SSE-S3,
  public-access-block, noncurrent-version expiry, and an OAC bucket policy
  conditioned on `AWS:SourceAccount` (not `AWS:SourceArn`, which would close a
  `site ↔ cdn` cycle);
- the **`artifacts` S3 bucket** (`modules/artifacts`) — private, versioned,
  SSE-S3, public-access-block, 365-day noncurrent-version expiry; it holds the
  sha-named Lambda zip the deploy applies;
- the **CloudFront distribution** (`modules/cdn`) — OAC origin, managed
  `CachingOptimized` / `SecurityHeadersPolicy`, `PriceClass_100`, the
  viewer-request rewrite function, `403`/`404` → `/404.html`, and the
  distribution-id SSM parameter that breaks the `cdn → api` cycle.

Everything above is `tofu validate`-clean and unapplied. The rewrite function's
rules are checked without AWS by `node --test modules/cdn/rewrite.test.mjs`.

## State bucket and backend

The bucket name follows PRD §10: `<project>-tfstate-<account-id>`; the env
roots compute it from `aws_caller_identity`. Each root declares a **partial S3
backend** (`backend.tf`), so the first bootstrap apply uses local state, then
the state moves into the bucket it just created:

```sh
cd envs/dev

# First bootstrap, on local state.
tofu init -backend=false
tofu apply

# Run the outputs, then fill in the account id.
cp backend.hcl.example backend.hcl
# bucket = "timeline-tfstate-<account-id>", key = "dev/terraform.tfstate",
# use_lockfile = true, encrypt = true

# Migrate local state into S3. Native S3 locking, no DynamoDB table.
tofu init -migrate-state -backend-config=backend.hcl
tofu plan -backend-config=backend.hcl   # no changes
```

Later runs and CI use `tofu init -backend-config=backend.hcl`. `backend.hcl`
is account-specific and gitignored; the committed `backend.hcl.example` is the
template.

## Deploy identity

`modules/bootstrap-iam` creates one OIDC provider and one `gha-deploy` role per
environment account. GitHub Actions assumes it with
`aws-actions/configure-aws-credentials`; the seam holds **no static AWS keys**.

Trust is scoped to exact `sub` claims (`:aud = sts.amazonaws.com`):

| Environment | Trusted subjects                                                                      |
| ----------- | ------------------------------------------------------------------------------------- |
| dev         | `repo:<org>/<repo>:ref:refs/heads/main`, `:pull_request`, `:environment:dev`          |
| prod        | `repo:<org>/<repo>:environment:prod` only                                             |

Org and repo come from `github_org` / `github_repo` in the env
`terraform.tfvars`; they are placeholders until the repository is settled.

The role is named `gha-deploy`. Its inline `app-iam` policy is limited to
`arn:aws:iam::<account>:role/<project>-<env>-*`, so it can manage only the
app's own roles (for example `timeline-dev-api`) and pass them to
`lambda.amazonaws.com`.

## Pipelines

### `ci.yml` — pull requests

`checks` (fmt, clippy `-D warnings`, test against the floci service container)
runs on every push and pull request. `infra-paths` detects changes under
`envs/` or `modules/`; `tofu-plan` then runs for non-fork PRs only:

1. assume the dev `gha-deploy` role via GitHub OIDC;
2. `tofu fmt -check -recursive`;
3. `tofu init` against the dev state bucket, `tofu validate`, and
   `tofu plan -lock=false` into the run log.

Fork PRs are skipped by design: they get no OIDC token, so the role cannot be
assumed. Plans are read-only and take no state lock.

### `deploy.yml` — main and v* tags

| Ref             | Job           | Environment              |
| --------------- | ------------- | ------------------------ |
| push to `main`  | `deploy-dev`  | `dev`                    |
| push to `v*` tag | `deploy-prod` | `prod` (required reviewers) |

Both deploy jobs follow the same PRD §11 sequence:

1. assume that account's `gha-deploy` role (OIDC, no static keys);
2. upload `dist/bootstrap.zip` to `s3://<artifacts>/api-<git-sha>.zip` and read
   back its `version_id`;
3. `tofu apply -var api_artifact_key=... -var api_artifact_version=...`;
4. run the `renderer` binary against that environment's DynamoDB;
5. `aws s3 sync <render>/ s3://<site>/ --delete --cache-control ...`;
6. `aws cloudfront create-invalidation /*` and wait for it to complete;
7. `curl` `/` and one page the render produced.

`deploy-prod` is not chained to `deploy-dev`: it runs from the tag's own run.
The build job runs on `ubuntu-24.04-arm`; the deploy jobs use the same label
because the renderer binary is aarch64.

**Constraint:** `aws s3 sync --delete` is the deploy's only upload path to the
`site` bucket. It mirrors rendered pages exactly, so cover images can never
live there — they live in `media`, which no deploy step touches.

The pipeline assumes each env root has been applied once by hand (the bootstrap
above), so the state bucket and artifacts bucket already exist. After that,
every release goes through `deploy.yml`; it no longer needs a laptop.

### GitHub Environments (variables only)

Set these per Environment (`dev`, `prod`); there are **no secrets**:

| Variable             | Value from                                                          |
| -------------------- | ------------------------------------------------------------------- |
| `AWS_DEPLOY_ROLE_ARN`| `tofu -chdir=envs/<env> output -raw gha_deploy_role_arn`            |
| `TF_STATE_BUCKET`    | `tofu -chdir=envs/<env> output -raw state_bucket_name`              |
| `ARTIFACTS_BUCKET`   | `tofu -chdir=envs/<env> output -raw artifacts_bucket_name`          |

The `prod` Environment also needs **required reviewers**; that review is the
only gate between a tag and production. `dev` has none.

Toolchain versions are pinned by `.opentofu-version` (read by
`opentofu/setup-opentofu`) and by `rust-toolchain.toml` for local runs; CI
passes the same Rust version to `dtolnay/rust-toolchain`. `Swatinem/rust-cache`
caches cargo.

## Rollback

Rollback is "re-run the previous tag": open **Actions → Deploy**, pick the run
for the tag to restore, and choose **Re-run all jobs**. It rebuilds that
commit, re-uploads the same `api-<git-sha>.zip` key in that environment's
artifacts bucket, applies it, and re-renders. Rendered pages are restored;
cover images in `media` are not touched, and that is correct.

Do not roll back by editing state, hand-syncing an old render, or renaming
objects. Emergency lever (PRD §11): repoint the `live` Lambda alias at the
previous published version and restore the previous S3 object versions.

Promotion is not a rebuild: dev and prod apply the same sha-named zip, so
"deploy what dev ran" means applying the same key in the prod account.

## Not verified here

The workflows are authored and YAML-linted, and the HCL is
`tofu validate`-clean, but nothing in this pipeline has run against AWS yet:
there is no account, OIDC provider, role, or state bucket to assume or read.
The first real run is the acceptance step for these tickets.
