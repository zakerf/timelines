#!/usr/bin/env bash
# One-command local dev: floci + Keycloak + seed + render + S3 website + API.
# The read/write split matches production: reads are S3 (floci emulates it, the
# same as CloudFront→S3), writes are the API. No dev-only code path.
#
# Usage:
#   scripts/dev.sh          start everything and run the API
#   scripts/dev.sh clean    stop containers and remove local state
#
# Environment:
#   FLOCI_PORT     host port for floci (default: 4566)
#   KEYCLOAK_PORT  host port for Keycloak (default: 8080)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUCKET="timeline-local-site"
SITE_DIR="target/site"
FLOCI_PORT="${FLOCI_PORT:-4566}"
KEYCLOAK_PORT="${KEYCLOAK_PORT:-8080}"
FLOCI_ENDPOINT="http://localhost:${FLOCI_PORT}"
KEYCLOAK_REALM="http://localhost:${KEYCLOAK_PORT}/realms/timeline"
export FLOCI_PORT KEYCLOAK_PORT

usage() {
  cat <<'EOF'
Usage: scripts/dev.sh [command]

Commands:
  dev, run, up   Start the backing services, seed, render, upload, and run the API
  clean          Stop the containers and remove local state (volumes + target/site)
  cleanup        Alias for clean
  -h, --help     Show this help

Environment:
  FLOCI_PORT     host port for floci (default: 4566)
  KEYCLOAK_PORT  host port for Keycloak (default: 8080)
EOF
}

cleanup() {
  echo "Stopping containers and removing local state..."
  docker compose down -v --remove-orphans
  rm -rf "$SITE_DIR"
  echo "Done."
}

# Container IDs belonging to this compose project, so a port held by our own
# already-running service is not reported as a conflict.
compose_container_ids() {
  docker compose ps -a -q 2>/dev/null || true
}

port_conflict() {
  local port="$1" ours id name
  ours="$(compose_container_ids)"
  while IFS='|' read -r id name; do
    [[ -n "$id" ]] || continue
    if ! grep -qx "$id" <<<"$ours"; then
      echo "Port $port is already in use by $name." >&2
      echo "Stop that container, or pick different ports:" >&2
      echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 bash scripts/dev.sh" >&2
      exit 1
    fi
  done < <(docker ps --filter "publish=$port" \
    --format '{{.ID}}|{{.Names}}' 2>/dev/null || true)
}

published_host_port() {
  docker compose port "$1" "$2" 2>/dev/null | head -n 1
}

# A container can be running but have no host port binding when an earlier
# start failed to allocate the port. Recreate just that service once.
ensure_published() {
  local service="$1" port="$2"
  if [[ -n "$(published_host_port "$service" "$port")" ]]; then
    return 0
  fi
  echo "Service '$service' is running but port $port is not published." >&2
  echo "Recreating '$service' to repair the port binding..." >&2
  if ! docker compose up -d --force-recreate "$service"; then
    echo "Could not recreate '$service'; the port may be held by another process." >&2
    echo "Run 'bash scripts/dev.sh clean' or pick different ports:" >&2
    echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 bash scripts/dev.sh" >&2
    exit 1
  fi
  if [[ -z "$(published_host_port "$service" "$port")" ]]; then
    echo "Could not publish $service:$port." >&2
    echo "Run 'bash scripts/dev.sh clean', then start again." >&2
    exit 1
  fi
}

command="${1:-dev}"
case "$command" in
  dev | run | up) ;;
  clean | cleanup | --clean)
    cleanup
    exit 0
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    echo "unknown command: $command" >&2
    usage >&2
    exit 2
    ;;
esac

port_conflict "$FLOCI_PORT"
port_conflict "$KEYCLOAK_PORT"

echo "Starting backing services (floci + Keycloak)..."
if ! docker compose up -d; then
  echo "" >&2
  echo "docker compose could not start the services." >&2
  echo "Ports ${FLOCI_PORT} (floci) and ${KEYCLOAK_PORT} (Keycloak) may be in use." >&2
  echo "Check with: docker ps --filter publish=${FLOCI_PORT}" >&2
  echo "Or pick different ports:" >&2
  echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 bash scripts/dev.sh" >&2
  exit 1
fi

ensure_published floci 4566
ensure_published keycloak 8080

echo "Waiting for floci + Keycloak to be healthy..."
healthy=0
for attempt in $(seq 1 90); do
  if curl -fsS "$FLOCI_ENDPOINT/_localstack/health" >/dev/null 2>&1 &&
    curl -fsS "$KEYCLOAK_REALM/.well-known/openid-configuration" >/dev/null 2>&1; then
    healthy=1
    break
  fi
  if [[ $((attempt % 15)) -eq 0 ]]; then
    echo "  still waiting ($((attempt * 2))s)..."
  fi
  sleep 2
done
if [[ "$healthy" -eq 0 ]]; then
  echo "floci or Keycloak did not become healthy; check 'docker compose logs'" >&2
  exit 1
fi

export AWS_ENDPOINT_URL="$FLOCI_ENDPOINT"
export AWS_DEFAULT_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=test
export AWS_SECRET_ACCESS_KEY=test
export KMS_KEY_ID=alias/timeline-local

echo "Seeding auth + demo timeline, rendering, uploading to S3..."
bash "$ROOT/scripts/seed-local-auth.sh" >/dev/null
cargo run -q -p renderer --bin seed -- --table timeline-data
cargo run -q -p renderer -- --table timeline-data --out "$SITE_DIR"

aws --endpoint-url "$FLOCI_ENDPOINT" s3 mb "s3://$BUCKET" >/dev/null 2>&1 || true
aws --endpoint-url "$FLOCI_ENDPOINT" s3 sync "$SITE_DIR/" "s3://$BUCKET/" --delete
aws --endpoint-url "$FLOCI_ENDPOINT" s3 website "s3://$BUCKET/" --index-document index.html --error-document 404.html

echo ""
echo "Ready."
echo "  Visit a timeline:  http://${BUCKET}.s3-website-eu-central-1.localhost:${FLOCI_PORT}/timeline/ohio-senate-2026.html"
echo "  Sign in:           http://localhost:8081/api/auth/login   (demo / demo)"
echo "  Publish:           http://localhost:8081/api/timeline"
echo "  Cleanup:           bash scripts/dev.sh clean"
echo ""
cargo run -q -p api
