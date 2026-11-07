output "state_bucket_name" {
  description = "Name of this environment's OpenTofu state bucket."
  value       = module.bootstrap.state_bucket_name
}

output "gha_deploy_role_arn" {
  description = "ARN of the GitHub Actions deploy role assumed by CI."
  value       = module.bootstrap_iam.gha_deploy_role_arn
}
