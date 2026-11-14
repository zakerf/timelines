#!/usr/bin/env bash
# End-to-end smoke against the nginx single origin (tickets 16 + 17): browse ->
# log in -> vote -> publish -> upload a cover -> read the new page, all from one
# URL.
#
# Run after `bash scripts/dev.sh`, which starts floci + Keycloak + nginx, seeds,
# renders, and runs the API in the foreground. Run this in a second terminal.
#
# Usage:
#   scripts/smoke.sh                 against http://localhost:8000
#   SITE_PORT=18000 scripts/smoke.sh  against a custom site port
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SITE_PORT="${SITE_PORT:-8000}"
BASE="http://localhost:${SITE_PORT}"
USERNAME="demo"
PASSWORD="demo"

# A 1x1 transparent PNG, so the cover upload exercises the real image path.
PNG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

fail() { echo "smoke: $*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/smoke.sh [-h]

Runs the end-to-end smoke against the nginx single origin: browse, log in,
vote, publish, upload a cover, and read the new page. Run `bash scripts/dev.sh`
first.

Environment:
  SITE_PORT   host port of the single origin (default: 8000)
EOF
}

case "${1:-}" in
  -h | --help) usage; exit 0 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
esac

echo "1. home page serves a timeline link..."
home="$(curl -fsS "$BASE/")" || fail "home page did not return 200"
grep -q 'href="/timeline/' <<<"$home" || fail "home page has no timeline link"

echo "2. a missing path returns the not-found page..."
status="$(curl -sS -o "$tmpdir/404" -w '%{http_code}' "$BASE/timeline/definitely-missing")"
[[ "$status" == "404" ]] || fail "missing path returned $status, want 404"
grep -q 'Page not found' "$tmpdir/404" || fail "missing path did not return the not-found page"

echo "3. the vendored HTMX asset is served from the same origin..."
asset="$(curl -fsS "$BASE/assets/htmx.min.js")" || fail "htmx asset did not return 200"
grep -q 'htmx' <<<"$asset" || fail "htmx asset is not the runtime"

echo "4. log in as $USERNAME..."
bash "$ROOT/scripts/keycloak-login.sh" \
  --base "$BASE" --username "$USERNAME" --password "$PASSWORD" \
  --session-file "$tmpdir/session" >/dev/null || fail "login failed"
SESSION="$(cat "$tmpdir/session")"
[[ -n "$SESSION" ]] || fail "login produced no session cookie"

me="$(curl -fsS -H "Cookie: timeline_session=$SESSION" "$BASE/api/auth/me")" \
  || fail "/api/auth/me failed"
grep -q '"authenticated":true' <<<"$me" || fail "not authenticated after login"
echo "   authenticated: $me"

echo "5. cast a vote..."
vote="$(curl -fsS -X POST "$BASE/api/timeline/ohio-senate-2026/vote" \
  -H "Cookie: timeline_session=$SESSION" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data "direction=up")" || fail "vote failed"
grep -q 'data-vote-target' <<<"$vote" || fail "vote did not return an HTMX fragment"
echo "   vote fragment returned"

echo "6. publish a timeline..."
publish="$(curl -fsS -X POST "$BASE/api/timeline" \
  -H "Cookie: timeline_session=$SESSION" \
  -H "Content-Type: application/json" \
  --data '{"title":"Smoke Test","description":"Published by scripts/smoke.sh","events":[{"date":"2026-01-01","title":"A dated fact","description":"One sourced event.","sources":["https://example.com/"]}]}')" \
  || fail "publish failed"
new_id="$(sed -n 's/.*data-timeline-id="\([^"]*\)".*/\1/p' <<<"$publish")"
[[ -n "$new_id" ]] || fail "publish fragment carried no timeline id"

echo "7. upload a cover and serve it from /media/..."
printf '%s' "$PNG_B64" | base64 -d > "$tmpdir/cover.png"
cover="$(curl -fsS -X POST "$BASE/api/timeline/$new_id/cover" \
  -H "Cookie: timeline_session=$SESSION" \
  -F "file=@$tmpdir/cover.png" \
  -F "credit=Smoke")" || fail "cover upload failed"
media_key="$(sed -n 's/.*src="\/media\/\([^"]*\)".*/\1/p' <<<"$cover")"
[[ -n "$media_key" ]] || fail "cover fragment carried no media key"
curl -fsS "$BASE/media/$media_key" >/dev/null || fail "cover was not served from /media/"
echo "   cover served: /media/$media_key"

echo "8. read the new page through the single origin..."
page="$(curl -fsS "$BASE/timeline/$new_id")" || fail "new timeline page did not return 200"
grep -q 'Smoke Test' <<<"$page" || fail "new timeline page does not show the title"
grep -q '/assets/htmx.min.js' <<<"$page" || fail "new page is not wired to the local HTMX"

echo ""
echo "smoke ok: browse -> login -> vote -> publish -> cover -> read all work from $BASE"
