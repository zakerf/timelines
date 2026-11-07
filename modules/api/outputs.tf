output "function_name" {
  description = "Name of the api Lambda function."
  value       = aws_lambda_function.api.function_name
}

output "function_arn" {
  description = "ARN of the api Lambda function (unqualified)."
  value       = aws_lambda_function.api.arn
}

output "alias_name" {
  description = "Name of the live alias API Gateway targets."
  value       = aws_lambda_alias.live.name
}

output "alias_arn" {
  description = "ARN of the live alias."
  value       = aws_lambda_alias.live.arn
}

output "log_group_name" {
  description = "CloudWatch log group for the api Lambda."
  value       = aws_cloudwatch_log_group.api.name
}

output "api_id" {
  description = "HTTP API id."
  value       = aws_apigatewayv2_api.api.id
}

output "api_endpoint" {
  description = "HTTP API invoke URL (origin for the CloudFront /api/* behavior)."
  value       = aws_apigatewayv2_api.api.api_endpoint
}

output "api_domain_name" {
  description = "HTTP API host name, without the scheme."
  value       = replace(aws_apigatewayv2_api.api.api_endpoint, "https://", "")
}

output "api_execution_arn" {
  description = "HTTP API execution ARN."
  value       = aws_apigatewayv2_api.api.execution_arn
}

output "api_stage_name" {
  description = "HTTP API stage name ($default)."
  value       = aws_apigatewayv2_stage.default.name
}
