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
  time.

Everything above is `tofu validate`-clean and unapplied.

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

Root outputs to copy into GitHub Environment **variables** (never secrets):
`gha_deploy_role_arn` and `state_bucket_name`.

## Not verified here

Ticket 02 also asks for `tofu plan` against dev from CI via OIDC assumption.
That requires the real dev account, its OIDC provider and the role; it cannot
be exercised locally and is left for the deploy-pipeline ticket once accounts
exist.
