output "state_bucket_name" {
  description = "Name of the OpenTofu remote-state bucket."
  value       = aws_s3_bucket.state.bucket
}
