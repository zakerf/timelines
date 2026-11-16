# AGENTS.md

Timeline-based news site: one Cargo workspace (3 crates) + in-repo OpenTofu.

`docs/LOCAL_DEV.md` owns local setup, ports and env overrides; `docs/INFRA.md` owns OpenTofu, the arm64 Lambda pipelines and rollback (re-run the previous tag — state and renders stay untouched); `modules/README.md` maps the Terraform modules. Read the matching one before touching that area.

## Crates

- `crates/data` — DynamoDB item model + key builders: no HTTP, no S3. `keys.rs` is the **only** place key strings are assembled; timestamps are fixed-width UTC ISO-8601 millis so sort keys order lexicographically. Every row carries an `itemType` discriminator plus `PK`/`SK`.
- `crates/renderer` — Leptos 0.8 static HTML generator (SSG) + HTMX. The lib is reused by the API's write path, not just its own binary.
- `crates/api` — axum router mounted at `/api/*`, packaged as the Lambda `bootstrap`. Owns every write.

Rust edition 2024.

## Commands

`bash scripts/dev.sh` starts floci, Keycloak, nginx, seeds a Timeline, renders and runs the API; `bash scripts/dev.sh clean` tears down containers, volumes and `target/site`.

Daily checks, in CI order (`docs/LOCAL_DEV.md` §4–5 for the OpenTofu half):

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace
node --test modules/cdn/rewrite.test.mjs
```

## floci tests

`cargo test --workspace` stays green with nothing running: the floci tests are `#[ignore]`d **and** feature-gated. The `[[test]]` entries in `crates/api/Cargo.toml` are the complete list — the docs describe a subset. Endpoint comes from `AWS_ENDPOINT_URL` (unset on real AWS), region `eu-central-1`, creds `test`/`test`.

```sh
docker compose up -d floci
cargo test -p data --features fixture -- --ignored
cargo test -p api --features floci -- --ignored
cargo test -p api --features floci --test vote_floci -- --ignored   # a single test
```

`_routes.rs` tests are plain unit/route tests, only `_floci.rs` need the emulator.

## Renderer

- Call `renderer::init_executor()` before any Leptos reactive API — Leptos has no one-shot SSG executor (`crates/renderer/src/executor.rs`).
- Pages ship no hydration bundle; the only scripts are the vendored HTMX (`crates/renderer/assets/htmx.min.js`) and the site script (`crates/renderer/assets/app.js`, section-numbered: account chrome, vote upgrades, confirmation, report widget).
- The standalone binary writes `.html` to a local dir, while the API write path uses `renderer::publish_routes` to `PUT` to S3.

## Write path

Publish / cover / suggestion / collection mutations each own the whole slice in-process: DynamoDB transaction → Leptos render → S3 `PUT` → CloudFront invalidation. The shared step is `crates/api/src/render.rs`.

## Worktrees

Each worktree carries its own multi-gigabyte `target/`, so the agent that creates one removes it once the work is merged or abandoned:

```sh
git worktree remove --force <dir>   # --force is required: target/ is untracked
```

Commit or stash first, since `--force` discards uncommitted work; `git worktree prune` clears entries left by a hand-deleted directory. Builds you are not running stay untouched, and `CARGO_TARGET_DIR` stays per-worktree: a shared one serializes concurrent builds on Cargo's lock and turns one `cargo clean` into a wipe of every worktree.
