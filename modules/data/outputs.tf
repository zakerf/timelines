output "table_name" {
  description = "Name of the single DynamoDB table."
  value       = aws_dynamodb_table.data.name
}

output "table_arn" {
  description = "ARN of the single DynamoDB table."
  value       = aws_dynamodb_table.data.arn
}

output "throttle_alarm_name" {
  description = "Name of the DynamoDB throttle alarm."
  value       = aws_cloudwatch_metric_alarm.throttles.alarm_name
}
