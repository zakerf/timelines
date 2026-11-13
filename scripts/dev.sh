#!/usr/bin/env bash
# One-command local dev: floci + Keycloak + seed + render + S3 website + API.
# The read/write split matches production: reads are S3 (floci emulates it, the
# same as CloudFront→S3), writes are the API. No dev-only code path.
set -euo pipefail
BUCKET="timeline-local-site"

echo "Starting backing services (floci + Keycloak)..."
docker compose up -d

echo "Waiting for floci + Keycloak to be healthy..."
for _ in $(seq 1 90); do
  if curl -fsS http://localhost:4566/_localstack/health >/dev/null 2>&1 \
     && curl -fsS http://localhost:8080/realms/timeline/.well-known/openid-configuration >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

export AWS_ENDPOINT_URL=http://localhost:4566
export AWS_DEFAULT_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=test
export AWS_SECRET_ACCESS_KEY=test
export KMS_KEY_ID=alias/timeline-local

echo "Seeding auth + demo timeline, rendering, uploading to S3..."
python scripts/seed-local-auth.py >/dev/null
cargo run -q -p renderer --bin seed -- --table timeline-data
cargo run -q -p renderer -- --table timeline-data --out target/site

aws --endpoint-url http://localhost:4566 s3 mb "s3://$BUCKET" >/dev/null 2>&1 || true
aws --endpoint-url http://localhost:4566 s3 sync target/site/ "s3://$BUCKET/" --delete
aws --endpoint-url http://localhost:4566 s3 website "s3://$BUCKET/" --index-document index.html --error-document 404.html

echo ""
echo "Ready."
echo "  Visit a timeline:  http://${BUCKET}.s3-website-eu-central-1.localhost:4566/timeline/ohio-senate-2026.html"
echo "  Sign in:           http://localhost:8081/api/auth/login   (demo / demo)"
echo "  Publish:           http://localhost:8081/api/timeline"
echo ""
cargo run -q -p api
