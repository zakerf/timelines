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

variable "table_name" {
  description = "Name of the single DynamoDB table."
  type        = string
}

variable "deletion_protection_enabled" {
  description = "Enable table deletion protection (prod only per PRD §10)."
  type        = bool
}

variable "sns_topic_arn" {
  description = "SNS topic that receives the DynamoDB throttle alarm. Null disables alarm actions."
  type        = string
  default     = null
}

variable "throttle_alarm_threshold" {
  description = "ThrottledRequests sum in one minute that trips the alarm."
  type        = number
  default     = 1
}
