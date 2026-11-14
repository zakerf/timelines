#!/usr/bin/env bash
# Seed the local floci state the auth seam needs.
#
# Creates the single DynamoDB table (same key layout as `modules/data`), writes
# the OIDC, distribution-id, and IP-salt SSM parameters, and creates the local
# KMS key alias used for refresh-token encryption. Idempotent: re-running it is
# safe.
#
# Requires the AWS CLI v2, curl, and floci (`docker compose up -d floci`).
# Request bodies go through `--cli-input-json` files in a temp dir, so shell
# quoting cannot mangle the OIDC JSON.
#
# Usage:
#     scripts/seed-local-auth.sh [--endpoint http://localhost:4566]
set -euo pipefail

ENDPOINT="${AWS_ENDPOINT_URL:-http://localhost:4566}"
TABLE_NAME="timeline-data"
OIDC_PARAM="/timeline/dev/oidc"
DISTRIBUTION_PARAM="/timeline/dev/cloudfront/distribution-id"
IP_SALT_PARAM="/timeline/dev/ip-salt"
IP_SALT_VALUE="timeline-local-ip-salt"
KMS_ALIAS="alias/timeline-local"
KEYCLOAK_PORT="${KEYCLOAK_PORT:-8080}"
OIDC_BASE="http://localhost:${KEYCLOAK_PORT}/realms/timeline"
OIDC_JSON="$(printf '{"discovery_url":"%s/.well-known/openid-configuration","authorization_url":"%s/protocol/openid-connect/auth","token_url":"%s/protocol/openid-connect/token","jwks_url":"%s/protocol/openid-connect/certs","client_id":"timelines","token_endpoint_auth_method":"none"}' "$OIDC_BASE" "$OIDC_BASE" "$OIDC_BASE" "$OIDC_BASE")"

usage() {
  cat <<'EOF'
Usage: scripts/seed-local-auth.sh [--endpoint URL]

  --endpoint URL   floci endpoint (default: $AWS_ENDPOINT_URL or http://localhost:4566)
  KEYCLOAK_PORT    Keycloak port used in the OIDC URLs (default: 8080)
  -h, --help       show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --endpoint)
      if [[ $# -lt 2 ]]; then
        echo "--endpoint needs a value" >&2
        exit 2
      fi
      ENDPOINT="$2"
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

for tool in aws curl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 1
  fi
done

export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-test}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-test}"
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-eu-central-1}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

aws_cli() {
  aws --endpoint-url "$ENDPOINT" "$@"
}

file_url() {
  local path="$1"
  if command -v cygpath >/dev/null 2>&1; then
    path="$(cygpath -m "$path")"
  fi
  printf 'file://%s' "$path"
}

aws_input() {
  local service="$1" operation="$2" body="$3"
  local file="$tmpdir/input.json"
  printf '%s\n' "$body" >"$file"
  aws_cli "$service" "$operation" --cli-input-json "$(file_url "$file")" >/dev/null
}

put_parameter() {
  local name="$1" value="$2" type="${3:-String}" escaped
  # JSON-escape the value and pass it as a string, not as a nested object.
  escaped="$(printf '%s' "$value" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
  aws_input ssm put-parameter \
    "$(printf '{"Name":"%s","Type":"%s","Value":"%s","Overwrite":true}' \
      "$name" "$type" "$escaped")"
}

create_table_body() {
  cat <<'JSON'
{
  "TableName": "timeline-data",
  "BillingMode": "PAY_PER_REQUEST",
  "AttributeDefinitions": [
    {"AttributeName": "PK", "AttributeType": "S"},
    {"AttributeName": "SK", "AttributeType": "S"},
    {"AttributeName": "GSI1PK", "AttributeType": "S"},
    {"AttributeName": "GSI1SK", "AttributeType": "S"},
    {"AttributeName": "GSI2PK", "AttributeType": "S"},
    {"AttributeName": "GSI2SK", "AttributeType": "S"}
  ],
  "KeySchema": [
    {"AttributeName": "PK", "KeyType": "HASH"},
    {"AttributeName": "SK", "KeyType": "RANGE"}
  ],
  "GlobalSecondaryIndexes": [
    {
      "IndexName": "GSI1",
      "KeySchema": [
        {"AttributeName": "GSI1PK", "KeyType": "HASH"},
        {"AttributeName": "GSI1SK", "KeyType": "RANGE"}
      ],
      "Projection": {"ProjectionType": "ALL"}
    },
    {
      "IndexName": "GSI2",
      "KeySchema": [
        {"AttributeName": "GSI2PK", "KeyType": "HASH"},
        {"AttributeName": "GSI2SK", "KeyType": "RANGE"}
      ],
      "Projection": {"ProjectionType": "ALL"}
    }
  ]
}
JSON
}

if ! curl -fsS "$ENDPOINT/_localstack/health" >/dev/null 2>&1; then
  echo "floci is not reachable at $ENDPOINT" >&2
  exit 1
fi

if aws_cli dynamodb describe-table --table-name "$TABLE_NAME" >/dev/null 2>&1; then
  echo "table $TABLE_NAME already exists"
else
  aws_input dynamodb create-table "$(create_table_body)"
  echo "created table $TABLE_NAME"
fi

put_parameter "$OIDC_PARAM" "$OIDC_JSON"
echo "wrote $OIDC_PARAM"

put_parameter "$DISTRIBUTION_PARAM" "local-distribution"
echo "wrote $DISTRIBUTION_PARAM"

put_parameter "$IP_SALT_PARAM" "$IP_SALT_VALUE" "SecureString"
echo "wrote $IP_SALT_PARAM"

key_id="$(aws_cli kms describe-key --key-id "$KMS_ALIAS" \
  --query 'KeyMetadata.KeyId' --output text 2>/dev/null || true)"
if [[ -z "$key_id" || "$key_id" == "None" ]]; then
  key_id="$(aws_cli kms create-key \
    --description "timeline local refresh-token key" \
    --query 'KeyMetadata.KeyId' --output text)"
  aws_cli kms create-alias \
    --alias-name "$KMS_ALIAS" \
    --target-key-id "$key_id" >/dev/null
fi

echo "KMS key $key_id at $KMS_ALIAS"
echo
echo "Run the API with:"
echo "  KMS_KEY_ID=$KMS_ALIAS cargo run -p api"
