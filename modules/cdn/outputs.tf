output "distribution_id" {
  description = "CloudFront distribution id."
  value       = aws_cloudfront_distribution.cdn.id
}

output "distribution_domain_name" {
  description = "CloudFront-assigned domain name."
  value       = aws_cloudfront_distribution.cdn.domain_name
}

output "distribution_id_param_name" {
  description = "SSM parameter the API Lambda reads for the distribution id."
  value       = aws_ssm_parameter.distribution_id.name
}
