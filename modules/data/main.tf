locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }
}

# PRD §6: one table, every item carries an `itemType` discriminator.
# PRD §10: on-demand billing, PITR on, TTL on, deletion protection prod-only,
# GSI1 + GSI2, no Streams, no global tables.
resource "aws_dynamodb_table" "data" {
  name         = var.table_name
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "PK"
  range_key    = "SK"

  deletion_protection_enabled = var.deletion_protection_enabled
  stream_enabled              = false

  point_in_time_recovery {
    enabled = true
  }

  ttl {
    attribute_name = "expiresAt"
    enabled        = true
  }

  attribute {
    name = "PK"
    type = "S"
  }

  attribute {
    name = "SK"
    type = "S"
  }

  attribute {
    name = "GSI1PK"
    type = "S"
  }

  attribute {
    name = "GSI1SK"
    type = "S"
  }

  attribute {
    name = "GSI2PK"
    type = "S"
  }

  attribute {
    name = "GSI2SK"
    type = "S"
  }

  # Owner queue: sparse, present only while a suggestion is PENDING.
  global_secondary_index {
    name            = "GSI1"
    hash_key        = "GSI1PK"
    range_key       = "GSI1SK"
    projection_type = "ALL"
  }

  # Collection membership, including the ROOT pseudo-collection.
  global_secondary_index {
    name            = "GSI2"
    hash_key        = "GSI2PK"
    range_key       = "GSI2SK"
    projection_type = "ALL"
  }

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "throttles" {
  alarm_name          = "${var.table_name}-throttles"
  alarm_description   = "DynamoDB requests are being throttled for ${var.table_name}."
  namespace           = "AWS/DynamoDB"
  metric_name         = "ThrottledRequests"
  dimensions          = { TableName = aws_dynamodb_table.data.name }
  statistic           = "Sum"
  period              = 60
  evaluation_periods  = 1
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.throttle_alarm_threshold
  treat_missing_data  = "notBreaching"

  alarm_actions = var.sns_topic_arn == null ? [] : [var.sns_topic_arn]

  tags = local.tags
}
