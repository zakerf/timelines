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

### Data integration test (floci)

`crates/data` owns the single-table item model and key builders. Its floci
smoke test creates the same table the `data` OpenTofu module builds (`PK`/`SK`,
GSI1, GSI2) and exercises the timeline/event, owner-queue, and own-vote read
paths against the emulator.

```sh
docker compose up -d floci
cargo test -p data --features fixture --test floci_smoke -- --ignored
```

The test is marked `#[ignore]` and requires the `fixture` feature, so plain
`cargo test --workspace` stays green with no floci running. It reads
`AWS_ENDPOINT_URL` (default `http://localhost:4566`), `AWS_DEFAULT_REGION`
(default `eu-central-1`), and dummy credentials (`test`/`test`), creates a
process-unique table, and deletes it afterwards.

## 3. Run the binaries

`crates/data` is a library. The two binaries are still stubs in this ticket:

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
tofu -chdir=envs/prod init -backend=false
tofu -chdir=envs/prod validate
```

`modules/` holds local modules; `envs/dev` and `envs/prod` are the roots.
`bootstrap`, `bootstrap-iam`, and `data` are wired; the remaining modules arrive
in later tickets. `fmt` and `validate` need no AWS credentials. The pieces that
only exist against a real account (state bucket, OIDC provider, IAM role) and
the one-time state migration are documented in [`INFRA.md`](INFRA.md).

## Teardown

```sh
docker compose down -v
```
