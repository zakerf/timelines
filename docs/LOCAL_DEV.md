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
| floci    | http://localhost:4566   | AWS-shaped services (DynamoDB, S3, KMS, ...) |
| Keycloak | http://localhost:8080   | OIDC provider, dev mode only               |

Keycloak imports `keycloak/realm-timeline.json` on first start: realm `timeline`,
public PKCE client `timeline-site`, and user `demo` / `demo`. Because Keycloak
owns port 8080, the API runs on **8081** (see below).

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

The render run also writes `target/site/404.html`, the CloudFront custom error
page for both origin `403` and `404` (PRD §4). It is deliberately not indexed.

Seeding is idempotent; it overwrites the demo rows. The seed binary creates the
table with the same key layout as the `data` OpenTofu module (PK/SK, GSI1,
GSI2) when it does not exist.

### Serve the rendered site locally (read path)

Production serves `target/site/` from a private S3 bucket through CloudFront.
Locally, floci's S3 static-website hosting is the closest match. With floci
running and the environment from §2 exported:

```sh
# 1. Render (see §3). The demo page is target/site/timeline/ohio-senate-2026.html.
cargo run -p renderer --bin seed -- --table timeline-data
cargo run -p renderer -- --table timeline-data --out target/site

# 2. Create a local bucket, upload the render, enable website hosting.
#    --endpoint-url is explicit so any AWS CLI v2 works; AWS_ENDPOINT_URL also
#    works on CLI v2.13+.
aws --endpoint-url http://localhost:4566 s3 mb s3://timeline-local-site
aws --endpoint-url http://localhost:4566 s3 sync target/site/ s3://timeline-local-site/
aws --endpoint-url http://localhost:4566 s3 website s3://timeline-local-site/ \
  --index-document index.html --error-document 404.html
```

Open the rendered page:

```text
http://timeline-local-site.s3-website-eu-central-1.localhost:4566/timeline/ohio-senate-2026.html
```

The same commands work in PowerShell (backtick instead of `\` for line
continuation). Re-run the `s3 sync` after each render; the bucket is a mirror
of `target/site/`, exactly like the deploy job.

**Extensionless-URL caveat.** The `/timeline/<id>` → `/timeline/<id>.html`
rewrite is a CloudFront Function, and floci does not run CloudFront Functions.
Locally, browse the `.html` URL directly. S3 website hosting still resolves
`index.html` for `/` and slash-terminated prefixes and serves
`404.html` for misses.

**Fallback: any static file server.** If the S3 emulation is unavailable, serve
the directory directly. Extensionless URLs will not resolve there either, so
open the `.html` path:

```sh
python -m http.server 8000 --directory target/site
# http://localhost:8000/timeline/ohio-senate-2026.html
```

The CloudFront distribution itself (`modules/cdn`) is real-AWS-only. Locally it
is HCL-validated and its rewrite function is unit-tested:

```sh
node --test modules/cdn/rewrite.test.mjs
```

### Renderer executor

Leptos has no built-in executor for one-shot SSG. `crates/renderer/src/executor.rs`
installs a custom one: `spawn` goes to `tokio::spawn`, `spawn_local` goes to an
undriven futures `LocalPool` (static HTML has no browser to update). The binary
installs it before the route-list walk. The module docs explain the two panic
modes this avoids ("no reactor running" and `spawn_local` outside a `LocalSet`).

### API front door (write path)

`crates/api` is an axum server. Locally it runs as a plain HTTP process; the
same router is packaged as the Lambda `bootstrap` for deployment. The HTTP API
`$default` stage adds no path prefix, so every route lives under `/api/*` in
both places.

With the environment from §2 exported:

```sh
cargo run -p api
# INFO api listening (plain HTTP) bind_addr=127.0.0.1:8081
```

In another terminal:

```sh
curl -i http://127.0.0.1:8081/api/health
# HTTP/1.1 200 OK
# {"status":"ok","version":"0.1.0"}
```

The local base path is `http://127.0.0.1:8081/api`. Set `BIND_ADDR` to move the
listener, and `PUBLIC_BASE_URL` to change the origin the OIDC redirect URI is
built from (it must match the Keycloak client's registered redirect URI).

| Variable                    | Local default                                | Purpose                                     |
| --------------------------- | -------------------------------------------- | ------------------------------------------- |
| `DDB_TABLE`                 | `timeline-data`                              | single DynamoDB table                       |
| `SITE_BUCKET`               | `timeline-local-site`                        | rendered-pages bucket                       |
| `CF_DISTRIBUTION_ID_PARAM`  | `/timeline/dev/cloudfront/distribution-id`   | distribution id read at init                |
| `OIDC_CONFIG_PARAM`         | `/timeline/dev/oidc`                         | OIDC JSON written by `modules/config`       |
| `IP_SALT_PARAM`             | `/timeline/dev/ip-salt`                      | out-of-band SecureString salt, by name only |
| `SESSION_COOKIE_NAME`       | `timeline_session`                           | opaque session cookie                       |
| `SESSION_TTL_DAYS`          | `30`                                         | session TTL in days                         |
| `PUBLIC_BASE_URL`           | `http://localhost:8081`                      | origin the browser uses; builds redirect URI |
| `KMS_KEY_ID`                | unset (local ciphertext fallback)            | KMS key/alias for refresh-token encryption  |
| `RUST_LOG`                  | `info`                                       | tracing filter                              |

`/api/health` is liveness only: no DynamoDB or SSM call, so it returns 200 even
with floci stopped. At startup the process *tries* to resolve the SSM-backed
configuration (distribution id + OIDC JSON) through the AWS SDK, which honours
`AWS_ENDPOINT_URL`; if floci is absent it logs a warning and keeps serving.
Auth resolves the OIDC parameter lazily, so a missing distribution-id parameter
does not block login.

The OIDC parameter is a JSON `String` (see `modules/config`):

```json
{
  "discovery_url": "http://localhost:8080/realms/timeline/.well-known/openid-configuration",
  "authorization_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/auth",
  "token_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/token",
  "jwks_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/certs",
  "client_id": "timeline-site",
  "token_endpoint_auth_method": "none"
}
```

### Auth: login through Keycloak

The seam is PRD §7 exactly: public client, authorization-code + PKCE, no client
secret. `/api/auth/login` stores the PKCE verifier server-side (a short-TTL
`AUTHSTATE#<state>` item), binds `state` to the browser with a short-lived
cookie, and redirects to Keycloak; `/api/auth/callback` checks both, exchanges
the code, verifies the ID token against the realm JWKS
(RS256, `iss`, `aud`, `exp`, `token_use` when present), upserts
`ACCOUNT#<sub>`, and sets an opaque `HttpOnly`/`Secure`/`SameSite=Lax` cookie
backed by a TTL'd `SESSION#<id>` item. `/api/auth/logout` deletes the session,
revokes the refresh token, and clears the cookie. `/api/auth/me` exposes the
signed-in identity (anonymous requests get `{"authenticated":false}`).

**1. Start the backing services and wait for the realm.**

```sh
docker compose up -d
curl -fsS http://localhost:8080/realms/timeline/.well-known/openid-configuration
```

**2. Seed floci: table, OIDC + distribution SSM parameters, KMS key alias.**

```sh
python scripts/seed-local-auth.py
```

The script prints the KMS alias. It is idempotent; re-run it freely. (It uses
`--cli-input-json` files under the hood so PowerShell quoting cannot mangle the
OIDC JSON.)

**3. Run the API with KMS.**

```sh
# bash
KMS_KEY_ID=alias/timeline-local cargo run -p api
```

```powershell
$env:KMS_KEY_ID = "alias/timeline-local"
cargo run -p api
```

Without `KMS_KEY_ID` the refresh token is stored as `local:<base64>` instead of
`kms:<ciphertext>`; everything else behaves identically. floci has a real KMS
service, so prefer the alias.

**4. Log in.**

Open <http://localhost:8081/api/auth/login> in a browser and sign in as
`demo` / `demo`. The browser lands on `http://localhost:8081/` (a 303) with the
session cookie set.

Or script the identical flow with the stdlib-only helper:

```sh
python scripts/keycloak-login.py
# 1. /api/auth/login -> http://localhost:8080/realms/timeline/...
# 4. /api/auth/callback -> 303 /
#    Set-Cookie: timeline_session=...; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax
# 5. /api/auth/me -> {"authenticated":true,"account":{"id":"...","display_name":"Demo User"}}
# 6. POST /api/auth/logout -> 303 timeline_session=; ... Max-Age=0 ...
```

The script treats localhost as a secure context so the `Secure` cookie is
round-tripped, exactly as a browser does.

**5. Confirm the rows in floci.**

```sh
aws --endpoint-url http://localhost:4566 dynamodb scan \
  --table-name timeline-data \
  --query "Items[?itemType.S=='ACCOUNT']"
```

The new row is `PK = ACCOUNT#<sub>` with `displayName`, `createdAt`,
`lastSeenAt`, and `refreshTokenCiphertext` (`kms:...`). While signed in a
matching `SESSION#<id>` row exists; after logout it is gone. The
`AUTHSTATE#<state>` row is deleted by the callback (single use) and is TTL'd
otherwise.

### Auth integration test (floci)

The floci test covers the account upsert (including `createdAt` preservation),
SESSION create/read/delete, the single-use AUTHSTATE row, and a real KMS
encrypt/decrypt round-trip.

```sh
docker compose up -d floci
cargo test -p api --features floci --test auth_floci -- --ignored
```


## 4. Run the checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
node --test modules/cdn/rewrite.test.mjs
```

CI runs the Rust checks (`.github/workflows/ci.yml`). The CloudFront rewrite
test has no AWS dependency; run it anywhere Node 22 is installed.

## 5. OpenTofu

```sh
tofu fmt -check -recursive
tofu -chdir=envs/dev init -backend=false
tofu -chdir=envs/dev validate
tofu -chdir=envs/prod init -backend=false
tofu -chdir=envs/prod validate
```

`modules/` holds local modules; `envs/dev` and `envs/prod` are the roots.
`bootstrap`, `bootstrap-iam`, `data`, `site`, `cdn`, `config`, and `api` are
wired; `artifacts` and `media` arrive in later tickets. `fmt` and `validate`
need no AWS credentials. The pieces that only exist against a real account (state bucket,
OIDC provider, IAM role, S3 site bucket, CloudFront distribution/function) and
the one-time state migration are documented in [`INFRA.md`](INFRA.md).

## Teardown

```sh
docker compose down -v
```
