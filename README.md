# timeline-site

Timeline-based news site. One cargo workspace, two binaries (`renderer` for read-path HTML, `api` for Lambda mutations), OpenTofu infra in-repo.

The implementation-ready spec this repo is built from: `plans/timeline-site/PRD.md` in the shared `ai` repo (`C:\Users\Zaker\Desktop\repos\ai`). Build tickets live beside it under `plans/timeline-site/build/issues/`.

## Local dev

`docker compose up` brings up floci (AWS-shaped services, port 4566) and Keycloak (OIDC). The app and renderer run against `AWS_ENDPOINT_URL=http://localhost:4566`. See the ticket 01 scaffold for the exact setup.
