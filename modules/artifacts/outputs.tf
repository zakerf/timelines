output "bucket_name" {
  description = "Name of the private Lambda artifact bucket."
  value       = aws_s3_bucket.artifacts.bucket
}

output "bucket_arn" {
  description = "ARN of the private Lambda artifact bucket."
  value       = aws_s3_bucket.artifacts.arn
}
