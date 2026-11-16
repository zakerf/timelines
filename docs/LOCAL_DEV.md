# Local development

Everything runs locally first: floci emulates the AWS services, Keycloak provides
the OIDC seam, nginx mirrors CloudFront as the single origin, and the Rust
binaries talk to floci and Keycloak.

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
| nginx    | http://localhost:8000   | single origin: pages, `/media/*`, `/api/*` |

nginx is the local stand-in for the CloudFront distribution: it serves the
rendered pages plus the `/media/*` and `/api/*` behaviors from one origin, so
the site is usable end to end at http://localhost:8000. Keycloak owns its own
port (8080 by default), so the API runs on **8081** (reachable through nginx or
directly).

If 4566, 8080, or 8000 is already in use, pick different host ports:

```sh
FLOCI_PORT=14566 KEYCLOAK_PORT=18080 SITE_PORT=18000 bash scripts/dev.sh
```

`dev.sh` passes `FLOCI_PORT`, `KEYCLOAK_PORT`, and `SITE_PORT` through to
Compose, health checks, the seed script's OIDC URLs, and the printed site URL.
Changing `SITE_PORT` off `8000` also requires registering the new origin's
redirect URI in `keycloak/realm-timeline.json` (the realm is pinned to `8000`
and `8081`); otherwise login redirects to an unregistered URI.

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

The render also writes the rest of the site: `index.html`, `timelines.html` (the
full list), one `collection/<id>.html` per Collection, the vendored assets
(`/assets/htmx.min.js`, `/assets/app.js`, `favicon.svg`, `/assets/og.png`), and
the crawler files `robots.txt` and `sitemap.xml` built from
`PUBLIC_BASE_URL`. It is standalone: inline CSS, baked vote counts, no hydration
bundle; the only script is the deferred HTMX asset. Open it from disk or sync it
to S3 (`aws s3 sync target/site/ s3://<site-bucket>/ --delete`).

The render run also writes `target/site/404.html`, the CloudFront custom error
page for both origin `403` and `404` (PRD §4). It is deliberately not indexed.

The library also has an S3 write mode (`renderer::publish_routes`) used by the
API's write path: it renders a requested path set and `PUT`s the documents to
the site bucket with `Cache-Control: max-age=86400`. The standalone binary
still writes to a local directory; see [Publish a Timeline](#publish-a-timeline-write-path).

Seeding is idempotent; it overwrites the demo rows. The seed binary creates the
table with the same key layout as the `data` OpenTofu module (PK/SK, GSI1,
GSI2) when it does not exist.

### Serve the rendered site locally (read path)

Production serves `target/site/` from a private S3 bucket through CloudFront.
Locally, the nginx service is the CloudFront stand-in: it proxies the read
pages from the floci S3 site bucket with the same extensionless rewrite, maps
missing objects to `404.html`, and serves `/media/*` and `/api/*` from the same
origin. `dev.sh` seeds, renders, syncs the bucket, and starts nginx:

```sh
bash scripts/dev.sh
```

Open the site end to end:

```text
http://localhost:8000/
http://localhost:8000/timeline/ohio-senate-2026
```

Extensionless URLs (`/`, `/collection/<id>`, `/timeline/<id>`) work out of the
box: nginx applies the same rewrite as the CloudFront Function in
`modules/cdn/rewrite.js`. The vendored HTMX bundle is served at
`/assets/htmx.min.js` by the same origin, so the read page's interactivity has
no network dependency.

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

With the environment from §2 exported (and, for the single origin, the
`BIND_ADDR` and `PUBLIC_BASE_URL` that `dev.sh` sets):

```sh
cargo run -p api
# INFO api listening (plain HTTP) bind_addr=0.0.0.0:8081
```

In another terminal:

```sh
curl -i http://127.0.0.1:8081/api/health
# HTTP/1.1 200 OK
# {"status":"ok","version":"0.1.0"}
```

The API is reached through nginx at `http://localhost:8000/api`, or directly at
`http://127.0.0.1:8081/api`. `dev.sh` binds the listener to `0.0.0.0:8081` so
nginx can reach it, and sets `PUBLIC_BASE_URL=http://localhost:8000` so the OIDC
redirect URI matches the single origin and the Keycloak client's registered
redirect URI.

| Variable                    | Local default                                | Purpose                                     |
| --------------------------- | -------------------------------------------- | ------------------------------------------- |
| `DDB_TABLE`                 | `timeline-data`                              | single DynamoDB table                       |
| `SITE_BUCKET`               | `timeline-local-site`                        | rendered-pages bucket                       |
| `MEDIA_BUCKET`              | `timeline-local-media`                       | immutable cover-image bucket                |
| `CF_DISTRIBUTION_ID_PARAM`  | `/timeline/dev/cloudfront/distribution-id`   | distribution id read at init                |
| `OIDC_CONFIG_PARAM`         | `/timeline/dev/oidc`                         | OIDC JSON written by `modules/config`       |
| `IP_SALT_PARAM`             | `/timeline/dev/ip-salt`                      | out-of-band SecureString salt, by name only |
| `IP_SALT`                   | unset (read the SSM parameter)               | local-only salt override for vote IP hashes |
| `SESSION_COOKIE_NAME`       | `timeline_session`                           | opaque session cookie                       |
| `SESSION_TTL_DAYS`          | `30`                                         | session TTL in days                         |
| `PUBLIC_BASE_URL`           | `http://localhost:8000`                      | origin the browser uses: redirect URI, canonical/OG/sitemap URLs |
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
  "client_id": "timelines",
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

**2. Seed floci: table, OIDC/distribution/IP-salt SSM parameters, KMS key alias.**

```sh
bash scripts/seed-local-auth.sh
```

The script prints the KMS alias. It is idempotent; re-run it freely. (It writes
`--cli-input-json` files to a temp dir, so shell quoting cannot mangle the OIDC
JSON.)

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

Open <http://localhost:8000/api/auth/login> in a browser and sign in as
`demo` / `demo`. The browser lands on the home page at `http://localhost:8000/`
(a 303) with the session cookie set on the single origin.

Or script the identical flow with the curl-based helper:

```sh
bash scripts/keycloak-login.sh --base http://localhost:8000
# 1. /api/auth/login -> http://localhost:8080/realms/timeline/...
# 4. /api/auth/callback -> 303 /
#    Set-Cookie: timeline_session=...; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax
# 5. /api/auth/me -> {"authenticated":true,"account":{"id":"...","display_name":"Demo User"}}
# 6. POST /api/auth/logout -> 303 timeline_session=; ... Max-Age=0 ...
```

The script treats localhost as a secure context so the `Secure` cookie is
round-tripped, exactly as a browser does. Pass `--session-file <path>` to write
the session cookie to a file and skip logout (the smoke script uses this).

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

### Votes (HTMX fragments)

A signed-in user votes at Timeline level or Event level:

```sh
# cast / change / withdraw (direction = up | down | none)
curl -i -X POST http://localhost:8000/api/timeline/<timelineId>/vote \
  -H "Cookie: timeline_session=<id>" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "direction=up"

curl -i -X POST http://localhost:8000/api/timeline/<timelineId>/event/<eventId>/vote \
  -H "Cookie: timeline_session=<id>" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "direction=none"

# own-vote state for a whole page: one Query on VOTE#<accountId>
curl http://localhost:8000/api/timeline/<timelineId>/votes -H "Cookie: timeline_session=<id>"
```

The mutating responses are `text/html` vote fragments (baked counts plus
`data-my-vote`), so HTMX replaces the widget without re-rendering the static
page. A self-vote returns `403`; a full fixed-window counter returns `429` with
`Retry-After`. IP limits use `sha256(ip + salt)` from
`CloudFront-Viewer-Address` only; a client `X-Forwarded-For` is ignored.

```sh
docker compose up -d floci
cargo test -p api --features floci --test vote_floci -- --ignored --nocapture
```

### Publish a Timeline (write path)

Publishing is the first real mutation and the whole vertical slice:
checklist → DynamoDB transaction → in-process Leptos render → S3 `PUT` →
exact-path invalidation → HTMX fragment. The API owns all of it, so no
separate renderer process is involved.

**1. Start the backing services, seed, and create the site bucket.**

```sh
docker compose up -d
bash scripts/seed-local-auth.sh

aws --endpoint-url http://localhost:4566 s3 mb s3://timeline-local-site
# "BucketAlreadyOwnedByYou" on a re-run is fine.
```

`scripts/seed-local-auth.sh` writes `/timeline/dev/cloudfront/distribution-id` as
`local-distribution`. That placeholder makes the API log the invalidation paths
and skip the AWS call; floci has no CloudFront anyway. The render → S3 `PUT`
half is real.

**2. Run the API.**

```sh
# bash
export AWS_ENDPOINT_URL=http://localhost:4566 AWS_DEFAULT_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test
KMS_KEY_ID=alias/timeline-local cargo run -p api
```

**3. Log in and publish through the form.**

Open <http://localhost:8000/api/timeline> and sign in as `demo` / `demo`. The
form posts to `POST /api/timeline` and the response is the HTMX fragment: the
new title, its Events, and a link to `/timeline/<id>`.

Events are one per line:

```text
YYYY-MM-DD | Title | https://source[,https://source2] | Description
YYYY-MM-DD | Title | https://source[,https://source2] | Description | crucial
```

The optional fifth part names the Event's significance: `minor`,
`regular`/`standard`, or `crucial`/`featured`; without it the Event is
`regular`. Any other fifth value stays part of the Description, so a `|` inside
a four-part Description is safe.

Every Event needs an exact date, a non-empty title, and at least one valid
`http(s)` source; the Timeline needs a title and at least one Event. A refused
publish is `400` with a per-field JSON error list:

```json
{"error":"checklist_failed","errors":[
  {"field":"title","message":"timeline title is required"},
  {"field":"events[0].date","message":"exact date as YYYY-MM-DD is required"}
]}
```

**4. The same publish over curl (JSON).**

Use the `timeline_session` cookie from step 3 (or `scripts/keycloak-login.sh`):

```sh
curl -i http://localhost:8000/api/timeline \
  -H 'content-type: application/json' \
  -H 'cookie: timeline_session=<session-id>' \
  --data '{
    "title": "Local Publish Demo",
    "description": "Written through the first mutation.",
    "collectionId": "elections-2026",
    "events": [
      {
        "date": "2026-02-04",
        "title": "Candidate filing deadline",
        "description": "Petitions were due.",
        "sources": ["https://www.ohiosos.gov/elections/candidates/"]
      }
    ]
  }'
```

A successful publish is `201 Created`, `content-type: text/html`, and
`cache-control: no-store`; the body is the fragment and carries
`data-timeline-id="<id>"`. The API has already:

- written `TIMELINE#<id>` with `ownerId`, `eventCount`, and UTC
  `createdAt`/`updatedAt`, plus the `EVENT#<timestamp>#<eventId>` rows, in one
  transaction;
- re-rendered `/timeline/<id>`, `/collection/<collectionId>` (when the Timeline
  is collected), and `/`, uploading `timeline/<id>.html`,
  `collection/<collectionId>.html`, and `index.html` to the site bucket;
- issued the invalidation path set, or logged it when the distribution id is
  the local placeholder.

**5. Verify the rows and the rendered pages.**

```sh
aws --endpoint-url http://localhost:4566 dynamodb scan \
  --table-name timeline-data \
  --query "Items[?itemType.S=='TIMELINE']"

aws --endpoint-url http://localhost:4566 s3 ls \
  s3://timeline-local-site/timeline/
```

Open the uploaded page through the single origin (extensionless URL):

```text
http://localhost:8000/timeline/<id>
```

The S3 object is the Spine page with the new Events baked in, because the API
renders with the same `renderer` library the standalone SSG binary uses.

**6. The publish integration test (floci).**

The test drives the authenticated `POST /api/timeline` route twice: a checklist
failure (`400` + per-field errors) and a valid publish. It then reads the
TIMELINE/EVENT rows, asserts `eventCount`, and fetches the rendered
`timeline/<id>.html`, `index.html`, and collection page from the S3 bucket.

```sh
docker compose up -d floci
cargo test -p api --features floci --test publish_floci -- --ignored
```

### Cover images (write path + `/media`)

A cover is the owner-uploaded hero image (PRD §9). Bytes are proxied through
the API as multipart, validated by magic number (JPEG/PNG/WebP; GIF and SVG are
refused), capped at 4 MiB, and stored as-is at an immutable
`cover/<timelineId>/<uuid>.<ext>` key in the separate **media** bucket. The
renderer emits `/media/<key>`; no cover means the deterministic gradient.

**1. Create the media bucket beside the site bucket** (once; re-runs are fine):

```sh
aws --endpoint-url http://localhost:4566 s3 mb s3://timeline-local-media
```

**2. Upload, replace, or remove as the owner.** With the API running and a
`timeline_session` cookie (see the publish section):

```sh
# Upload or replace. `credit` is optional plain-text attribution.
curl -i http://localhost:8000/api/timeline/<id>/cover \
  -H 'cookie: timeline_session=<session-id>' \
  -F 'file=@cover.png' \
  -F 'credit=Photo: Demo Author'

# Remove -> clear the attribute and drop back to the gradient.
curl -i -X DELETE http://localhost:8000/api/timeline/<id>/cover \
  -H 'cookie: timeline_session=<session-id>'
```

Both responses are the hero HTMX fragment with `cache-control: no-store`.
Upload writes the object with `content-type: image/png|jpeg|webp` and
`cache-control: public, max-age=31536000, immutable`, sets
`coverImage { key, width, height, credit }` on the Timeline, re-renders the
affected page set, and returns the hero. A replacement deletes the superseded
object best-effort; a removal deletes it the same way.

A browser asks for a document instead by sending `Accept: text/html` (that is
what the settings page's forms do): both routes then `303` back to
`/api/timeline/<id>/settings` (`?saved=cover` or `?saved=cover-removed`), and a
refusal is the settings page carrying the message rather than the fragment.

A refused upload is a `400` (not an accepted image, missing `file` field,
oversized `credit`) or `413` (over 4 MiB); a non-owner is `403` and anonymous is
`401`.

**3. Check the object and the page.**

```sh
aws --endpoint-url http://localhost:4566 s3 ls s3://timeline-local-media/cover/<id>/
aws --endpoint-url http://localhost:4566 dynamodb get-item \
  --table-name timeline-data \
  --key '{"PK":{"S":"TIMELINE#<id>"},"SK":{"S":"META"}}' \
  --query 'Item.coverImage'

# The baked page points at the immutable key.
aws --endpoint-url http://localhost:4566 s3 cp \
  s3://timeline-local-site/timeline/<id>.html - | Select-String '/media/cover/'
```

Locally, nginx serves the media object through the single origin at
`http://localhost:8000/media/cover/<id>/<uuid>.png`, mirroring the `/media/*`
CloudFront behavior (OAC, `CachingOptimized` + `SecurityHeadersPolicy`, no
`.html` rewrite).

**4. The cover integration test (floci).**

The test drives the real route: stranger `403`, owner upload (PNG dimensions
parsed, object metadata, attribute, re-rendered page), replacement (new key, old
object deleted), and removal (attribute gone, gradient hero).

```sh
docker compose up -d floci
cargo test -p api --features floci --test cover_floci -- --ignored
```

### Smoke test (end to end)

`scripts/smoke.sh` walks the full loop against the single origin — browse, log
in, vote, publish, upload and remove a cover from the owner's settings page,
create a Collection from the publish picker, rename it and move a Timeline
through its forms, report a Timeline, and read the owner's summary as both a
page and JSON. It is the acceptance bar for the whole local setup:

```sh
bash scripts/dev.sh          # terminal 1
bash scripts/smoke.sh        # terminal 2
```

It defaults to `http://localhost:8000`; override with `SITE_PORT`.

### Owner pages, suggestions and reports

The renderer's static pages carry the markup; these routes are the writes behind
them. All of them take the `timeline_session` cookie from the auth section.

| Route | Who | What it does |
| ----- | --- | ------------ |
| `GET\|POST /api/timeline/{id}/settings` | owner | Details, Collection, cover lifecycle, unpublish/publish, delete. The POST takes an `action` field: `save`, `remove-cover`, `unpublish`, `publish`, or `delete`, and answers `303` to `?saved=<outcome>`. |
| `GET /api/timeline/{id}/reports` | owner | Report summary for the Timeline and its Events. JSON by default; `Accept: text/html` returns the owner's page instead. |
| `POST /api/collections` | signed in | Create a Collection (JSON), the endpoint behind the picker's inline create. |
| `POST\|PATCH\|DELETE /api/collections/{id}` | creator | Rename/re-nest and delete. The urlencoded POST is the Collection page's own form (`action=rename` or `action=delete`). |
| `POST /api/collections/{id}/membership` | creator | The Collection page's add/remove form (`timelineId`, `action=remove`). |
| `PUT /api/timeline/{id}/collection` | owner | JSON membership move (`{"collectionId": "..."}` or `null`). A Collection id with no row is refused, so no phantom page is minted. |
| `GET /api/timeline/{id}/suggestions` | public | Pending edits for a Timeline. `POST` submits one (form or JSON), and a refused submission is the form again with per-field errors. |
| `GET /api/timeline/{id}/event/{eventId}/suggest`, `GET /api/timeline/{id}/events/suggest` | public | The pre-filled edit form and the "suggest a missing event" form. |
| `POST .../suggestions/{suggestionId}/approve`, `.../reject` | owner | Apply or reject. A browser form gets `303 /api/suggestions/queue?done=<outcome>`. Rejecting needs a note; without one it is a `400` page, not a server error. |
| `GET /api/suggestions/queue` | owner | The moderation queue. JSON for API clients; `Accept: text/html` returns the queue page. |
| `POST\|DELETE .../report` | signed in | Report or withdraw a Timeline, Event, or Suggestion. Reporting is anonymous to the owner: they see a count, and three reports flag the item. |
| `GET /api/timeline/{id}/access`, `GET /api/collections/{id}/access` | anyone | `{"canEdit": bool}`; the pages' owner controls are revealed from it, and the writes still check ownership themselves. |

Both summaries (queue and reports) decide between JSON and a document on the
`Accept` header alone, so a client that sends `Accept: application/json` keeps
the JSON contract.

`scripts/dev.sh` renders `robots.txt` and `sitemap.xml` like any other page, so
they can be checked through the single origin:
`http://localhost:8000/robots.txt` and `http://localhost:8000/sitemap.xml`.


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
Both roots are thin: `modules/stack` wires `bootstrap`, `bootstrap-iam`, `data`,
`site`, `media`, `artifacts`, `cdn`, `config`, and `api`, and each root only
pins its environment name. `fmt` and `validate`
need no AWS credentials. The pieces that only exist against a real account (state bucket,
OIDC provider, IAM role, S3 site/media buckets, CloudFront distribution/function) and
the one-time state migration are documented in [`INFRA.md`](INFRA.md).

## Teardown

Stop the containers, remove their volumes, and delete the local rendered site:

```sh
bash scripts/dev.sh clean
```

The equivalent containers-only command is:

```sh
docker compose down -v
```
