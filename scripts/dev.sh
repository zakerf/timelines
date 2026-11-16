#!/usr/bin/env bash
# One-command local dev: floci + Keycloak + nginx + seed + render + S3 + API.
# nginx is the local stand-in for CloudFront: one origin serving the rendered
# pages (from floci S3), /media/* covers, and /api/* writes. No dev-only path.
#
# Usage:
#   scripts/dev.sh          start everything and run the API
#   scripts/dev.sh clean    stop containers and remove local state
#
# Environment:
#   FLOCI_PORT     host port for floci (default: 4566)
#   KEYCLOAK_PORT  host port for Keycloak (default: 8080)
#   SITE_PORT      host port for the nginx single origin (default: 8000)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUCKET="timeline-local-site"
MEDIA_BUCKET="timeline-local-media"
SITE_DIR="target/site"
FLOCI_PORT="${FLOCI_PORT:-4566}"
KEYCLOAK_PORT="${KEYCLOAK_PORT:-8080}"
SITE_PORT="${SITE_PORT:-8000}"
FLOCI_ENDPOINT="http://localhost:${FLOCI_PORT}"
KEYCLOAK_REALM="http://localhost:${KEYCLOAK_PORT}/realms/timeline"
SITE_ORIGIN="http://localhost:${SITE_PORT}"
export FLOCI_PORT KEYCLOAK_PORT SITE_PORT

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
  SITE_PORT      host port for the nginx single origin (default: 8000)
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
  # compose ps -q prints full ids, so this listing must not truncate them.
  while IFS='|' read -r id name; do
    [[ -n "$id" ]] || continue
    if ! grep -qx "$id" <<<"$ours"; then
      echo "Port $port is already in use by $name." >&2
      echo "Stop that container, or pick different ports:" >&2
      echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 SITE_PORT=18000 bash scripts/dev.sh" >&2
      exit 1
    fi
  done < <(docker ps --no-trunc --filter "publish=$port" \
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
    echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 SITE_PORT=18000 bash scripts/dev.sh" >&2
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
port_conflict "$SITE_PORT"

echo "Starting backing services (floci + Keycloak + nginx)..."
if ! docker compose up -d; then
  echo "" >&2
  echo "docker compose could not start the services." >&2
  echo "Ports ${FLOCI_PORT} (floci), ${KEYCLOAK_PORT} (Keycloak), and ${SITE_PORT} (nginx) may be in use." >&2
  echo "Check with: docker ps --filter publish=${FLOCI_PORT}" >&2
  echo "Or pick different ports:" >&2
  echo "  FLOCI_PORT=14566 KEYCLOAK_PORT=18080 SITE_PORT=18000 bash scripts/dev.sh" >&2
  exit 1
fi

ensure_published floci 4566
ensure_published keycloak 8080
ensure_published nginx 80

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
# The API is reached by nginx through the Docker host gateway, so it must bind
# beyond loopback, and its OIDC callback must point at the single origin.
export BIND_ADDR=0.0.0.0:8081
export PUBLIC_BASE_URL="$SITE_ORIGIN"

echo "Seeding auth + demo timeline, rendering, uploading to S3..."
bash "$ROOT/scripts/seed-local-auth.sh" >/dev/null
cargo run -q -p renderer --bin seed -- --table timeline-data
cargo run -q -p renderer -- --table timeline-data --out "$SITE_DIR"

aws --endpoint-url "$FLOCI_ENDPOINT" s3 mb "s3://$BUCKET" >/dev/null 2>&1 || true
aws --endpoint-url "$FLOCI_ENDPOINT" s3 mb "s3://$MEDIA_BUCKET" >/dev/null 2>&1 || true
aws --endpoint-url "$FLOCI_ENDPOINT" s3 sync "$SITE_DIR/" "s3://$BUCKET/" --delete

echo "Starting the API..."
cargo run -q -p api &
api_pid=$!
trap 'kill "$api_pid" 2>/dev/null || true' EXIT INT TERM

# The site itself is served by nginx/floci, so the page loads before the API is
# listening. Wait for the write path before telling the user it is ready.
echo "Waiting for the API to become healthy..."
api_ready=0
for attempt in $(seq 1 600); do
  if ! kill -0 "$api_pid" 2>/dev/null; then
    echo "The API exited before it became healthy; check the output above." >&2
    exit 1
  fi
  if curl -fsS "http://localhost:8081/api/health" >/dev/null 2>&1; then
    api_ready=1
    break
  fi
  if [[ $((attempt % 30)) -eq 0 ]]; then
    echo "  still waiting for the API (${attempt}s)..." >&2
  fi
  sleep 1
done
if [[ "$api_ready" -eq 0 ]]; then
  echo "The API did not become healthy at http://localhost:8081/api/health." >&2
  exit 1
fi

echo ""
echo "Ready."
echo "  Site:              ${SITE_ORIGIN}/"
echo "  A timeline:        ${SITE_ORIGIN}/timeline/ohio-senate-2026"
echo "  Sign in:           ${SITE_ORIGIN}/api/auth/login   (demo / demo)"
echo "  Publish:           ${SITE_ORIGIN}/api/timeline"
echo "  Cleanup:           bash scripts/dev.sh clean"
echo ""
wait "$api_pid"
