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

## 3. Seed and render a Timeline

`crates/renderer` is a Leptos 0.8 static-site generator. It reads a Timeline and
its ordered Events from DynamoDB, renders `/timeline/<id>` as a complete HTML
document, and writes it with `static_file_path` to `<out>/timeline/<id>.html`.

With floci running and the environment from §2 exported:

```sh
# Create the table if needed, then write the demo Timeline + 3 Events.
cargo run -p renderer --bin seed -- --table timeline-data

# Render every Timeline in the table.
cargo run -p renderer -- --table timeline-data --out target/site

# ...or just one.
cargo run -p renderer -- --table timeline-data --timeline ohio-senate-2026 --out target/site
```

The demo Timeline (`ohio-senate-2026`) has no cover, so its hero falls back to
the deterministic gradient keyed on the Timeline id. When a `coverImage` is on
the item, the hero renders `/media/<key>` with width/height attributes instead.

The rendered page lands at:

- `target/site/timeline/ohio-senate-2026.html`

It is standalone: inline CSS, baked vote counts, no hydration bundle, and no
`<script>` tags. Open it from disk or sync it to S3
(`aws s3 sync target/site/ s3://<site-bucket>/ --delete`).

Seeding is idempotent; it overwrites the demo rows. The seed binary creates the
table with the same key layout as the `data` OpenTofu module (PK/SK, GSI1,
GSI2) when it does not exist.

### Renderer executor

Leptos has no built-in executor for one-shot SSG. `crates/renderer/src/executor.rs`
installs a custom one: `spawn` goes to `tokio::spawn`, `spawn_local` goes to an
undriven futures `LocalPool` (static HTML has no browser to update). The binary
installs it before the route-list walk. The module docs explain the two panic
modes this avoids ("no reactor running" and `spawn_local` outside a `LocalSet`).

`crates/api` is still a stub; it arrives in a later ticket.

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
