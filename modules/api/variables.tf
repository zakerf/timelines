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

variable "account_id" {
  description = "AWS account id the Lambda and API Gateway live in."
  type        = string
}

variable "table_name" {
  description = "Name of the single DynamoDB table."
  type        = string
}

variable "table_arn" {
  description = "ARN of the single DynamoDB table."
  type        = string
}

variable "site_bucket_name" {
  description = "Name of the rendered-pages S3 bucket the API writes to."
  type        = string
}

variable "site_bucket_arn" {
  description = "ARN of the rendered-pages S3 bucket."
  type        = string
}

variable "media_bucket_name" {
  description = "Name of the cover-image S3 bucket (optional until the media module exists)."
  type        = string
  default     = null
}

variable "media_bucket_arn" {
  description = "ARN of the cover-image S3 bucket (optional until the media module exists)."
  type        = string
  default     = null
}

variable "distribution_id_param_name" {
  description = "SSM parameter the Lambda reads to break the cdn -> api cycle."
  type        = string
}

variable "oidc_config_param_name" {
  description = "SSM parameter holding the OIDC seam's JSON configuration."
  type        = string
}

variable "ip_salt_param_name" {
  description = "Out-of-band SecureString parameter holding the IP-hash salt."
  type        = string
}

variable "artifact_bucket_name" {
  description = "S3 bucket holding the packaged Lambda artifact."
  type        = string
}

variable "artifact_key" {
  description = "S3 object key of the packaged Lambda artifact."
  type        = string
  default     = "api.zip"
}

variable "artifact_version" {
  description = "S3 object version of the artifact (immutable sha-named keys)."
  type        = string
  default     = null
}

variable "session_cookie_name" {
  description = "Opaque HttpOnly session cookie name."
  type        = string
  default     = "timeline_session"
}

variable "session_ttl_days" {
  description = "Session lifetime in days; written onto the SESSION item TTL."
  type        = number
  default     = 30
}

variable "log_level" {
  description = "RUST_LOG value for the Lambda."
  type        = string
  default     = "info"
}

variable "memory_size" {
  description = "Lambda memory in MB (PRD §10: 256)."
  type        = number
  default     = 256
}

variable "timeout_seconds" {
  description = "Lambda timeout in seconds (PRD §10: 10)."
  type        = number
  default     = 10
}

variable "alarm_sns_topic_arn" {
  description = "SNS topic that receives operational alarms. Null disables alarm actions."
  type        = string
  default     = null
}

variable "alarm_threshold" {
  description = "Sum of errors/throttles/5xx in one minute that trips the alarm."
  type        = number
  default     = 1
}
