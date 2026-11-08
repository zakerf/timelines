#!/usr/bin/env python3
"""Seed the local floci state the auth seam needs.

Creates the single DynamoDB table (same key layout as `modules/data`), writes
the OIDC and distribution-id SSM parameters, and creates a local KMS key alias
used for refresh-token encryption. Idempotent: re-running it is safe.

Requires the AWS CLI v2 and floci (`docker compose up -d floci`). It passes
request bodies through `--cli-input-json` files because PowerShell mangles
embedded quotes when a JSON string is handed to a native command.

Usage:
    python scripts/seed-local-auth.py [--endpoint http://localhost:4566]
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import urllib.request

TABLE_NAME = "timeline-data"
OIDC_PARAM = "/timeline/dev/oidc"
DISTRIBUTION_PARAM = "/timeline/dev/cloudfront/distribution-id"
IP_SALT_PARAM = "/timeline/dev/ip-salt"
IP_SALT_VALUE = "timeline-local-ip-salt"
KMS_ALIAS = "alias/timeline-local"


def aws(endpoint, *args, check=True):
    # Force JSON: a user's ~/.aws/config may set output = yaml.
    command = ["aws", "--endpoint-url", endpoint, "--output", "json", *args]
    return subprocess.run(command, check=check, capture_output=True, text=True)


def aws_with_input(endpoint, service, operation, body, check=True):
    with tempfile.NamedTemporaryFile(
        "w", suffix=".json", delete=False, encoding="utf-8"
    ) as handle:
        json.dump(body, handle)
        path = handle.name
    try:
        return aws(
            endpoint,
            service,
            operation,
            "--cli-input-json",
            "file://" + path.replace("\\", "/"),
            check=check,
        )
    finally:
        os.unlink(path)


def table_exists(endpoint):
    result = aws(
        endpoint,
        "dynamodb",
        "describe-table",
        "--table-name",
        TABLE_NAME,
        check=False,
    )
    return result.returncode == 0


def create_table(endpoint):
    aws_with_input(
        endpoint,
        "dynamodb",
        "create-table",
        {
            "TableName": TABLE_NAME,
            "BillingMode": "PAY_PER_REQUEST",
            "AttributeDefinitions": [
                {"AttributeName": name, "AttributeType": "S"}
                for name in ("PK", "SK", "GSI1PK", "GSI1SK", "GSI2PK", "GSI2SK")
            ],
            "KeySchema": [
                {"AttributeName": "PK", "KeyType": "HASH"},
                {"AttributeName": "SK", "KeyType": "RANGE"},
            ],
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": name,
                    "KeySchema": [
                        {"AttributeName": f"{name}PK", "KeyType": "HASH"},
                        {"AttributeName": f"{name}SK", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                }
                for name in ("GSI1", "GSI2")
            ],
        },
    )


def put_parameter(endpoint, name, value, parameter_type="String"):
    aws_with_input(
        endpoint,
        "ssm",
        "put-parameter",
        {
            "Name": name,
            "Type": parameter_type,
            "Value": value,
            "Overwrite": True,
        },
    )


def ensure_kms_key(endpoint):
    described = aws(
        endpoint,
        "kms",
        "describe-key",
        "--key-id",
        KMS_ALIAS,
        check=False,
    )
    if described.returncode == 0:
        metadata = json.loads(described.stdout)["KeyMetadata"]
        return metadata["KeyId"]

    created = aws(
        endpoint,
        "kms",
        "create-key",
        "--description",
        "timeline local refresh-token key",
    )
    key_id = json.loads(created.stdout)["KeyMetadata"]["KeyId"]
    aws(
        endpoint,
        "kms",
        "create-alias",
        "--alias-name",
        KMS_ALIAS,
        "--target-key-id",
        key_id,
    )
    return key_id


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--endpoint",
        default=os.environ.get("AWS_ENDPOINT_URL", "http://localhost:4566"),
    )
    args = parser.parse_args()

    os.environ.setdefault("AWS_ACCESS_KEY_ID", "test")
    os.environ.setdefault("AWS_SECRET_ACCESS_KEY", "test")
    os.environ.setdefault("AWS_DEFAULT_REGION", "eu-central-1")

    try:
        with urllib.request.urlopen(
            args.endpoint + "/_localstack/health", timeout=5
        ) as response:
            response.read()
    except OSError as error:
        print(f"floci is not reachable at {args.endpoint}: {error}", file=sys.stderr)
        return 1

    if table_exists(args.endpoint):
        print(f"table {TABLE_NAME} already exists")
    else:
        create_table(args.endpoint)
        print(f"created table {TABLE_NAME}")

    put_parameter(
        args.endpoint,
        OIDC_PARAM,
        json.dumps(
            {
                "discovery_url": "http://localhost:8080/realms/timeline/.well-known/openid-configuration",
                "authorization_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/auth",
                "token_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/token",
                "jwks_url": "http://localhost:8080/realms/timeline/protocol/openid-connect/certs",
                "client_id": "timeline-site",
                "token_endpoint_auth_method": "none",
            }
        ),
    )
    print(f"wrote {OIDC_PARAM}")

    put_parameter(args.endpoint, DISTRIBUTION_PARAM, "local-distribution")
    print(f"wrote {DISTRIBUTION_PARAM}")

    put_parameter(args.endpoint, IP_SALT_PARAM, IP_SALT_VALUE, "SecureString")
    print(f"wrote {IP_SALT_PARAM}")

    key_id = ensure_kms_key(args.endpoint)
    print(f"KMS key {key_id} at {KMS_ALIAS}")
    print()
    print("Run the API with:")
    print(f'  KMS_KEY_ID={KMS_ALIAS} cargo run -p api')
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
