variable "project" {
  description = "Project slug used for tags and resource naming."
  type        = string
}

variable "env" {
  description = "Environment name (dev or prod)."
  type        = string
}

variable "region" {
  description = "AWS region the environment lives in."
  type        = string
}

variable "parameter_name" {
  description = "SSM parameter name holding the OIDC seam's JSON configuration."
  type        = string
}

variable "oidc_discovery_url" {
  description = "OIDC discovery document URL."
  type        = string
}

variable "oidc_authorization_url" {
  description = "OIDC authorization endpoint URL."
  type        = string
}

variable "oidc_token_url" {
  description = "OIDC token endpoint URL."
  type        = string
}

variable "oidc_jwks_url" {
  description = "OIDC JWKS URL used to verify ID tokens."
  type        = string
}

variable "oidc_client_id" {
  description = "Public OIDC client id (no client secret; PKCE protects the exchange)."
  type        = string
}

variable "token_endpoint_auth_method" {
  description = "OIDC token endpoint authentication method."
  type        = string
  default     = "none"

  validation {
    condition = contains(
      ["none", "client_secret_basic", "client_secret_post", "private_key_jwt"],
      var.token_endpoint_auth_method,
    )
    error_message = "Unsupported OIDC token_endpoint_auth_method."
  }
}

variable "ip_salt_param_name" {
  description = "Name of the out-of-band SecureString IP-hash salt parameter (created outside OpenTofu)."
  type        = string
}
