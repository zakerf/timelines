output "state_bucket_name" {
  description = "Name of this environment's OpenTofu state bucket."
  value       = module.bootstrap.state_bucket_name
}

output "gha_deploy_role_arn" {
  description = "ARN of the GitHub Actions deploy role assumed by CI."
  value       = module.bootstrap_iam.gha_deploy_role_arn
}

output "data_table_name" {
  description = "Name of the single DynamoDB table."
  value       = module.data.table_name
}

output "data_table_arn" {
  description = "ARN of the single DynamoDB table."
  value       = module.data.table_arn
}

output "site_bucket_name" {
  description = "Name of the private rendered-pages bucket."
  value       = module.site.bucket_name
}

output "site_bucket_arn" {
  description = "ARN of the private rendered-pages bucket."
  value       = module.site.bucket_arn
}

output "cdn_distribution_id" {
  description = "CloudFront distribution id."
  value       = module.cdn.distribution_id
}

output "cdn_distribution_domain_name" {
  description = "CloudFront-assigned domain name for the read path."
  value       = module.cdn.distribution_domain_name
}

output "cdn_distribution_id_param_name" {
  description = "SSM parameter the API Lambda reads for the distribution id."
  value       = module.cdn.distribution_id_param_name
}
