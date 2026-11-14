# timelines

Timeline-based news site. One cargo workspace, two binaries (`renderer` for read-path HTML, `api` for Lambda mutations), OpenTofu infra in-repo.

The implementation-ready spec this repo is built from: `plans/timeline-site/PRD.md` in the shared `ai` repo (`C:\Users\Zaker\Desktop\repos\ai`). Build tickets live beside it under `plans/timeline-site/build/issues/`.

## Layout

- `crates/renderer` — Leptos 0.8 static HTML renderer (Spine read page).
- `crates/api` — axum server for the write-path front door (plain HTTP locally,
  Lambda `bootstrap` in AWS).
- `modules/`, `envs/dev`, `envs/prod` — OpenTofu skeleton.
- `docker-compose.yml` — floci (AWS) + Keycloak (OIDC) for local dev.

## Local dev

```sh
bash scripts/dev.sh
```

This starts floci and Keycloak, seeds floci, renders the demo site, uploads it to
the local S3 bucket, and runs the API. Stop and clean everything with:

```sh
bash scripts/dev.sh clean
```

If ports 4566 or 8080 are already taken, set `FLOCI_PORT` / `KEYCLOAK_PORT`:

```sh
FLOCI_PORT=14566 KEYCLOAK_PORT=18080 bash scripts/dev.sh
```

floci serves AWS-shaped services on port 4566 and Keycloak on 8080. The app and
renderer always reach AWS through `AWS_ENDPOINT_URL=http://localhost:4566`
locally. Full steps, including the individual commands and checks, are in
[`docs/LOCAL_DEV.md`](docs/LOCAL_DEV.md).
