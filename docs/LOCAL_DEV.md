# Local development

Everything runs locally first: floci emulates the AWS services, Keycloak provides
the OIDC seam, and the Rust binaries talk to both.

## Prerequisites

- Docker with Compose v2
- Rust (pinned by `rust-toolchain.toml`; `rustup` installs it on first use)
- OpenTofu (optional for now; version pinned in `.opentofu-version`)

## 1. Start the backing services

```sh
docker compose up -d
```

This starts:

| Service  | URL                     | Purpose                                    |
| -------- | ----------------------- | ------------------------------------------ |
| floci    | http://localhost:4566   | AWS-shaped services (DynamoDB, S3, ...)    |
| Keycloak | http://localhost:8080   | OIDC provider, dev mode only               |

Wait for floci to report healthy, then check it:

```sh
docker compose ps
curl -fsS http://localhost:4566/_localstack/health
```

Keycloak admin console: http://localhost:8080 (`admin` / `admin`). It runs in
`start-dev` mode; production auth remains an open seam.

## 2. Point the app at floci

Every process that talks to AWS reads the same convention:

```sh
export AWS_ENDPOINT_URL=http://localhost:4566
export AWS_DEFAULT_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=test
export AWS_SECRET_ACCESS_KEY=test
```

PowerShell equivalent:

```powershell
$env:AWS_ENDPOINT_URL = "http://localhost:4566"
$env:AWS_DEFAULT_REGION = "eu-central-1"
$env:AWS_ACCESS_KEY_ID = "test"
$env:AWS_SECRET_ACCESS_KEY = "test"
```

`AWS_ENDPOINT_URL` is the single switch: locally it points at floci, in CI at
the floci service container, and in AWS it is unset. Never hard-code the
endpoint in application code.

Prove the endpoint works (needs the AWS CLI):

```sh
aws dynamodb list-tables
```

## 3. Run the binaries

The crates are stubs in this ticket:

```sh
cargo run -p renderer
cargo run -p api
```

`renderer` will render the static site off the write path; `api` will own every
mutation from inside Lambda. Both arrive in later tickets.

## 4. Run the checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three commands (`.github/workflows/ci.yml`).

## 5. OpenTofu

```sh
tofu fmt -check -recursive
tofu -chdir=envs/dev init -backend=false
tofu -chdir=envs/dev validate
```

`modules/` holds local modules; `envs/dev` and `envs/prod` are the roots.
`bootstrap` and `bootstrap-iam` are wired; the remaining modules arrive in
later tickets. `fmt` and `validate` need no AWS credentials. The pieces that
only exist against a real account (state bucket, OIDC provider, IAM role) and
the one-time state migration are documented in [`INFRA.md`](INFRA.md).

## Teardown

```sh
docker compose down -v
```
