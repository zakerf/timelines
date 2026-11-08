output "bucket_name" {
  description = "Name of the private cover-image bucket."
  value       = aws_s3_bucket.media.bucket
}

output "bucket_arn" {
  description = "ARN of the private cover-image bucket."
  value       = aws_s3_bucket.media.arn
}

output "bucket_regional_domain_name" {
  description = "Regional S3 domain used as the /media/* CloudFront origin."
  value       = aws_s3_bucket.media.bucket_regional_domain_name
}
