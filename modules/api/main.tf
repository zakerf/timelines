locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }

  function_name  = "${var.project}-${var.env}-api"
  api_name       = "${var.project}-${var.env}-api"
  log_group_name = "/aws/lambda/${local.function_name}"

  # PRD §10: s3:PutObject/DeleteObject on <site_bucket>/* and <media_bucket>/*.
  # media arrives with the cover-image ticket; until then only the site bucket.
  s3_object_resources = compact([
    "${var.site_bucket_arn}/*",
    var.media_bucket_arn == null ? "" : "${var.media_bucket_arn}/*",
  ])

  # PRD §10: ssm:GetParameter(s) on the OIDC + salt parameters only.
  config_parameter_arns = [
    "arn:aws:ssm:${var.region}:${var.account_id}:parameter${var.oidc_config_param_name}",
    "arn:aws:ssm:${var.region}:${var.account_id}:parameter${var.ip_salt_param_name}",
  ]

  alarm_actions = var.alarm_sns_topic_arn == null ? [] : [var.alarm_sns_topic_arn]
}

# PRD §10: a Lambda-created log group, 14-day retention, no X-Ray.
resource "aws_cloudwatch_log_group" "api" {
  name              = local.log_group_name
  retention_in_days = 14

  tags = local.tags
}

data "aws_iam_policy_document" "lambda_assume_role" {
  statement {
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }

    actions = ["sts:AssumeRole"]
  }
}

resource "aws_iam_role" "api" {
  name               = local.function_name
  assume_role_policy = data.aws_iam_policy_document.lambda_assume_role.json

  tags = local.tags
}

# PRD §10 inline policy. The cloudfront grant is account-scoped on purpose:
# the distribution id arrives from SSM, so IAM cannot name the distribution.
# The logs statement is required for the Lambda to write to its own log group.
resource "aws_iam_role_policy" "api" {
  name = "${local.function_name}-policy"
  role = aws_iam_role.api.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "DynamoDbTableAccess"
        Effect = "Allow"
        Action = [
          "dynamodb:GetItem",
          "dynamodb:PutItem",
          "dynamodb:UpdateItem",
          "dynamodb:DeleteItem",
          "dynamodb:Query",
          "dynamodb:TransactWriteItems",
          "dynamodb:BatchWriteItem",
        ]
        Resource = [var.table_arn, "${var.table_arn}/*"]
      },
      {
        Sid      = "SiteAndMediaObjects"
        Effect   = "Allow"
        Action   = ["s3:PutObject", "s3:DeleteObject"]
        Resource = local.s3_object_resources
      },
      {
        Sid      = "InvalidateDistribution"
        Effect   = "Allow"
        Action   = ["cloudfront:CreateInvalidation"]
        Resource = ["arn:aws:cloudfront::${var.account_id}:distribution/*"]
      },
      {
        Sid      = "ReadConfigParameters"
        Effect   = "Allow"
        Action   = ["ssm:GetParameter", "ssm:GetParameters"]
        Resource = local.config_parameter_arns
      },
      {
        Sid      = "DecryptSsmSecureString"
        Effect   = "Allow"
        Action   = ["kms:Decrypt"]
        Resource = ["arn:aws:kms:${var.region}:${var.account_id}:alias/aws/ssm"]
      },
      {
        Sid      = "WriteOwnLogs"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = ["${aws_cloudwatch_log_group.api.arn}:*"]
      },
    ]
  })
}

# PRD §10: provided.al2023 / arm64 / 256 MB / 10 s, published so the `live`
# alias is the rollback lever API Gateway targets.
resource "aws_lambda_function" "api" {
  function_name = local.function_name
  description   = "Timeline site write-path API"
  role          = aws_iam_role.api.arn

  runtime       = "provided.al2023"
  handler       = "bootstrap"
  architectures = ["arm64"]
  memory_size   = var.memory_size
  timeout       = var.timeout_seconds
  publish       = true

  s3_bucket         = var.artifact_bucket_name
  s3_key            = var.artifact_key
  s3_object_version = var.artifact_version

  environment {
    variables = {
      DDB_TABLE                = var.table_name
      SITE_BUCKET              = var.site_bucket_name
      CF_DISTRIBUTION_ID_PARAM = var.distribution_id_param_name
      OIDC_CONFIG_PARAM        = var.oidc_config_param_name
      IP_SALT_PARAM            = var.ip_salt_param_name
      SESSION_COOKIE_NAME      = var.session_cookie_name
      SESSION_TTL_DAYS         = tostring(var.session_ttl_days)
      RUST_LOG                 = var.log_level
    }
  }

  tracing_config {
    mode = "PassThrough"
  }

  logging_config {
    log_format = "Text"
    log_group  = aws_cloudwatch_log_group.api.name
  }

  depends_on = [
    aws_cloudwatch_log_group.api,
    aws_iam_role_policy.api,
  ]

  tags = local.tags
}

resource "aws_lambda_alias" "live" {
  name             = "live"
  description      = "Live traffic alias; repoint to an older published version for rollback."
  function_name    = aws_lambda_function.api.function_name
  function_version = aws_lambda_function.api.version
}

# PRD §10: HTTP API with a $default route and $default stage, payload 2.0.
# The v2 $default stage adds no path prefix, so the axum router owns /api/*.
resource "aws_apigatewayv2_api" "api" {
  name          = local.api_name
  protocol_type = "HTTP"
  description   = "Front door for the timeline site write path"

  tags = local.tags
}

resource "aws_apigatewayv2_integration" "lambda" {
  api_id                 = aws_apigatewayv2_api.api.id
  integration_type       = "AWS_PROXY"
  integration_uri        = aws_lambda_alias.live.invoke_arn
  payload_format_version = "2.0"
}

resource "aws_apigatewayv2_route" "default" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "$default"
  target    = "integrations/${aws_apigatewayv2_integration.lambda.id}"
}

resource "aws_apigatewayv2_stage" "default" {
  api_id      = aws_apigatewayv2_api.api.id
  name        = "$default"
  auto_deploy = true

  tags = local.tags
}

resource "aws_lambda_permission" "apigw" {
  statement_id  = "AllowApiGatewayInvoke"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api.function_name
  qualifier     = aws_lambda_alias.live.name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/*/*"
}

# PRD §10 observability: Lambda Errors/Throttles and API Gateway 5xx, all to
# the one SNS topic. No dashboards, no metric filters.
resource "aws_cloudwatch_metric_alarm" "errors" {
  alarm_name          = "${local.function_name}-errors"
  alarm_description   = "The api Lambda is returning errors."
  namespace           = "AWS/Lambda"
  metric_name         = "Errors"
  dimensions          = { FunctionName = aws_lambda_function.api.function_name }
  statistic           = "Sum"
  period              = 60
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = var.alarm_threshold
  treat_missing_data  = "notBreaching"

  alarm_actions = local.alarm_actions

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "throttles" {
  alarm_name          = "${local.function_name}-throttles"
  alarm_description   = "The api Lambda is being throttled."
  namespace           = "AWS/Lambda"
  metric_name         = "Throttles"
  dimensions          = { FunctionName = aws_lambda_function.api.function_name }
  statistic           = "Sum"
  period              = 60
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = var.alarm_threshold
  treat_missing_data  = "notBreaching"

  alarm_actions = local.alarm_actions

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "api_5xx" {
  alarm_name          = "${local.api_name}-5xx"
  alarm_description   = "The HTTP API is returning 5xx responses."
  namespace           = "AWS/ApiGateway"
  metric_name         = "5xx"
  dimensions          = { ApiId = aws_apigatewayv2_api.api.id }
  statistic           = "Sum"
  period              = 60
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = var.alarm_threshold
  treat_missing_data  = "notBreaching"

  alarm_actions = local.alarm_actions

  tags = local.tags
}
