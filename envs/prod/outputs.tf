# Re-exported from the shared stack module so CI and the deploy workflow keep
# reading one `tofu output` surface per environment.

output "state_bucket_name" {
  description = "Name of this environment's OpenTofu state bucket."
  value       = module.stack.state_bucket_name
}

output "gha_deploy_role_arn" {
  description = "ARN of the GitHub Actions deploy role assumed by CI."
  value       = module.stack.gha_deploy_role_arn
}

output "data_table_name" {
  description = "Name of the single DynamoDB table."
  value       = module.stack.data_table_name
}

output "data_table_arn" {
  description = "ARN of the single DynamoDB table."
  value       = module.stack.data_table_arn
}

output "site_bucket_name" {
  description = "Name of the private rendered-pages bucket."
  value       = module.stack.site_bucket_name
}

output "site_bucket_arn" {
  description = "ARN of the private rendered-pages bucket."
  value       = module.stack.site_bucket_arn
}

output "media_bucket_name" {
  description = "Name of the private cover-image bucket."
  value       = module.stack.media_bucket_name
}

output "media_bucket_arn" {
  description = "ARN of the private cover-image bucket."
  value       = module.stack.media_bucket_arn
}

output "artifacts_bucket_name" {
  description = "Name of the private Lambda artifact bucket."
  value       = module.stack.artifacts_bucket_name
}

output "cdn_distribution_id" {
  description = "CloudFront distribution id."
  value       = module.stack.cdn_distribution_id
}

output "cdn_distribution_domain_name" {
  description = "CloudFront-assigned domain name for the read path."
  value       = module.stack.cdn_distribution_domain_name
}

output "cdn_distribution_id_param_name" {
  description = "SSM parameter the API Lambda reads for the distribution id."
  value       = module.stack.cdn_distribution_id_param_name
}

output "oidc_config_param_name" {
  description = "SSM parameter holding the OIDC seam's JSON configuration."
  value       = module.stack.oidc_config_param_name
}

output "ip_salt_param_name" {
  description = "Out-of-band SecureString parameter holding the IP-hash salt."
  value       = module.stack.ip_salt_param_name
}

output "api_endpoint" {
  description = "HTTP API invoke URL (CloudFront /api/* origin)."
  value       = module.stack.api_endpoint
}

output "api_lambda_function_name" {
  description = "Name of the api Lambda function."
  value       = module.stack.api_lambda_function_name
}

output "api_lambda_alias_arn" {
  description = "ARN of the live alias API Gateway targets."
  value       = module.stack.api_lambda_alias_arn
}

output "alarm_sns_topic_arn" {
  description = "SNS topic every operational alarm publishes to."
  value       = module.stack.alarm_sns_topic_arn
}
