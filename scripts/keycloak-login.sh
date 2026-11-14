#!/usr/bin/env bash
# Drive the local OIDC authorization-code + PKCE flow with curl.
#
# This is the scripted equivalent of opening http://localhost:8081/api/auth/login
# in a browser: it follows the redirect to Keycloak, posts the demo credentials,
# hits /api/auth/callback, checks /api/auth/me with the cookie, then logs out.
#
# The cookies are HttpOnly/Secure/SameSite=Lax. Browsers treat localhost as a
# secure context, but curl refuses to send Secure cookies over plain HTTP. This
# script captures Set-Cookie itself and sends the Cookie header explicitly, so
# plain-HTTP localhost behaves like a browser.
#
# Usage:
#     scripts/keycloak-login.sh [--base http://localhost:8081] \
#         [--username demo] [--password demo]
set -euo pipefail

BASE="http://localhost:8081"
USERNAME="demo"
PASSWORD="demo"

usage() {
  cat <<'EOF'
Usage: scripts/keycloak-login.sh [--base URL] [--username NAME] [--password PASS]

  --base URL        API origin (default: http://localhost:8081)
  --username NAME   Keycloak user (default: demo)
  --password PASS   Keycloak password (default: demo)
  -h, --help        show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      [[ $# -ge 2 ]] || { echo "--base needs a value" >&2; exit 2; }
      BASE="$2"
      shift 2
      ;;
    --username)
      [[ $# -ge 2 ]] || { echo "--username needs a value" >&2; exit 2; }
      USERNAME="$2"
      shift 2
      ;;
    --password)
      [[ $# -ge 2 ]] || { echo "--password needs a value" >&2; exit 2; }
      PASSWORD="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done
BASE="${BASE%/}"

if ! command -v curl >/dev/null 2>&1; then
  echo "missing required tool: curl" >&2
  exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# Both Keycloak and the API run on localhost, so one cookie header serves both;
# cookies ignore ports. Later Set-Cookie values replace earlier ones.
declare -A COOKIES=()

capture_cookies() {
  local headers="$1" line pair name value
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    pair="$(printf '%s' "$line" | sed 's/^[^:]*:[[:space:]]*//')"
    name="${pair%%=*}"
    value="${pair#*=}"
    value="${value%%;*}"
    [[ -n "$name" ]] || continue
    if [[ -z "$value" ]]; then
      unset "COOKIES[$name]"
    else
      COOKIES["$name"]="$value"
    fi
  done < <(tr -d '\r' <"$headers" | grep -i '^set-cookie:' || true)
}

cookie_header() {
  local name out=""
  for name in "${!COOKIES[@]}"; do
    out+="${out:+; }$name=${COOKIES[$name]}"
  done
  printf '%s' "$out"
}

curl_cookies() {
  local header
  header="$(cookie_header)"
  if [[ -n "$header" ]]; then
    curl -sS -H "Cookie: $header" "$@"
  else
    curl -sS "$@"
  fi
}

header_value() {
  local headers="$1" name="$2"
  tr -d '\r' <"$headers" |
    grep -i "^${name}:" |
    tail -n 1 |
    sed 's/^[^:]*:[[:space:]]*//' || true
}

urljoin() {
  local base="$1" ref="$2"
  case "$ref" in
    http://* | https://*) printf '%s\n' "$ref" ;;
    /*)
      printf '%s\n' "$(printf '%s' "$base" | sed -E 's#^(https?://[^/]+).*#\1#')$ref"
      ;;
    *) printf '%s\n' "${base%/*}/$ref" ;;
  esac
}

# Follow a GET redirect chain by hand so every hop's Set-Cookie is captured.
# Sets GET_STATUS and GET_URL; the final body lands in $out. Called without a
# command substitution so capture_cookies can update the main-shell cookie map.
get_follow() {
  local url="$1" out="$2" headers="$3"
  local code location hops=0
  while :; do
    code="$(curl_cookies -o "$out" -D "$headers" -w '%{http_code}' "$url")"
    capture_cookies "$headers"
    case "$code" in
      301 | 302 | 303 | 307 | 308) ;;
      *) break ;;
    esac
    location="$(header_value "$headers" location)"
    [[ -n "$location" ]] || break
    url="$(urljoin "$url" "$location")"
    hops=$((hops + 1))
    [[ "$hops" -lt 10 ]] || break
  done
  GET_STATUS="$code"
  GET_URL="$url"
}

login_headers="$tmpdir/login.headers"
login_status="$(curl_cookies -o /dev/null -D "$login_headers" \
  -w '%{http_code}' "$BASE/api/auth/login")"
capture_cookies "$login_headers"
case "$login_status" in
  301 | 302 | 303 | 307 | 308) ;;
  *)
    echo "login did not redirect: $login_status" >&2
    exit 1
    ;;
esac
authorize_url="$(header_value "$login_headers" location)"
if [[ -z "$authorize_url" ]]; then
  echo "login redirect had no Location header" >&2
  exit 1
fi
echo "1. /api/auth/login -> $authorize_url"

page="$tmpdir/login.html"
get_follow "$authorize_url" "$page" "$tmpdir/page.headers"
page_status="$GET_STATUS"
current="$GET_URL"
echo "2. Keycloak login page at $current ($page_status)"

form_tag="$(tr '\n' ' ' <"$page" |
  grep -o '<form[^>]*id="kc-form-login"[^>]*>' || true)"
if [[ -z "$form_tag" ]]; then
  echo "no kc-form-login action in the login page" >&2
  exit 1
fi
action="$(printf '%s' "$form_tag" | sed -n 's/.*action="\([^"]*\)".*/\1/p')"
if [[ -z "$action" ]]; then
  echo "kc-form-login had no action attribute" >&2
  exit 1
fi
# Bash 5.2 treats '&' in a replacement as the match; escape it for a literal &.
action="${action//&amp;/\&}"
action="$(urljoin "$current" "$action")"
echo "3. POST credentials ($USERNAME) to $action"

data_args=()
while IFS= read -r input; do
  name="$(printf '%s' "$input" | sed -n 's/.*name="\([^"]*\)".*/\1/p')"
  [[ -n "$name" ]] || continue
  case "$name" in
    username | password) continue ;;
  esac
  value="$(printf '%s' "$input" | sed -n 's/.*value="\([^"]*\)".*/\1/p')"
  value="${value//&amp;/\&}"
  data_args+=(--data-urlencode "$name=$value")
done < <(tr '\n' ' ' <"$page" | grep -o '<input[^>]*>' || true)
data_args+=(--data-urlencode "username=$USERNAME" --data-urlencode "password=$PASSWORD")

post_headers="$tmpdir/post.headers"
code="$(curl_cookies -o "$tmpdir/post.body" -D "$post_headers" \
  -w '%{http_code}' "${data_args[@]}" "$action")"
capture_cookies "$post_headers"
next="$(header_value "$post_headers" location)"
reached_callback=0
for _ in $(seq 1 10); do
  case "$code" in
    301 | 302 | 303 | 307 | 308) ;;
    *) break ;;
  esac
  [[ -n "$next" ]] || break
  current="$(urljoin "$current" "$next")"
  post_headers="$tmpdir/hop.$code.headers"
  code="$(curl_cookies -o "$tmpdir/post.body" -D "$post_headers" \
    -w '%{http_code}' "$current")"
  capture_cookies "$post_headers"
  if [[ "$current" == *"/api/auth/callback"* ]]; then
    reached_callback=1
    break
  fi
  next="$(header_value "$post_headers" location)"
done

if [[ "$reached_callback" -eq 0 ]]; then
  echo "callback was not reached (status $code)" >&2
  head -c 2000 "$tmpdir/post.body" >&2 || true
  exit 1
fi

session="${COOKIES[timeline_session]:-}"
if [[ -z "$session" ]]; then
  echo "callback did not set timeline_session" >&2
  exit 1
fi

echo "4. /api/auth/callback -> $code $(header_value "$post_headers" location)"
tr -d '\r' <"$post_headers" |
  grep -i '^set-cookie:' |
  sed -E 's/^[Ss]et-[Cc]ookie:[[:space:]]*/   Set-Cookie: /' || true

me="$(curl_cookies "$BASE/api/auth/me")"
echo "5. /api/auth/me -> $me"

logout_headers="$tmpdir/logout.headers"
logout_status="$(curl_cookies -o /dev/null -D "$logout_headers" \
  -w '%{http_code}' -X POST "$BASE/api/auth/logout")"
capture_cookies "$logout_headers"
echo "6. POST /api/auth/logout -> $logout_status $(header_value "$logout_headers" set-cookie)"
