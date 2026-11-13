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

variable "site_bucket_regional_domain_name" {
  description = "Regional domain name of the rendered-pages bucket (CloudFront origin)."
  type        = string
}

variable "media_bucket_regional_domain_name" {
  description = "Regional domain name of the cover-image bucket (/media/* origin)."
  type        = string
}

variable "api_domain_name" {
  description = "Host name of the HTTP API (no scheme) for the /api/* origin."
  type        = string
}

variable "default_root_object" {
  description = "Object served for the distribution root."
  type        = string
  default     = "index.html"
}

variable "distribution_id_param_name" {
  description = "SSM parameter name where the distribution id is published for the API Lambda."
  type        = string
}

variable "alarm_sns_topic_arn" {
  description = "SNS topic that receives the CloudFront 5xx alarm. Null disables alarm actions."
  type        = string
  default     = null
}

variable "alarm_threshold" {
  description = "CloudFront 5xx error rate (percent, averaged over a minute) that trips the alarm."
  type        = number
  default     = 1
}
