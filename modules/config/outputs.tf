output "oidc_config_param_name" {
  description = "SSM parameter name holding the OIDC JSON configuration."
  value       = aws_ssm_parameter.oidc.name
}

output "oidc_config_param_arn" {
  description = "ARN of the OIDC configuration parameter."
  value       = aws_ssm_parameter.oidc.arn
}

output "ip_salt_param_name" {
  description = "Name of the out-of-band SecureString IP-hash salt parameter."
  value       = var.ip_salt_param_name
}
