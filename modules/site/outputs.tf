output "bucket_name" {
  description = "Name of the private rendered-pages bucket."
  value       = aws_s3_bucket.site.bucket
}

output "bucket_arn" {
  description = "ARN of the private rendered-pages bucket."
  value       = aws_s3_bucket.site.arn
}

output "bucket_regional_domain_name" {
  description = "Regional S3 domain used as the CloudFront origin."
  value       = aws_s3_bucket.site.bucket_regional_domain_name
}
