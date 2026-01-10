output "bucket_name" {
  description = "Name of the S3 bucket that stores disk images."
  value       = aws_s3_bucket.images.bucket
}

output "cloudfront_domain_name" {
  description = "CloudFront distribution domain name (e.g. d123abc.cloudfront.net)."
  value       = aws_cloudfront_distribution.images.domain_name
}

output "image_base_url" {
  description = "Recommended base URL for images (ends with /<image_prefix>/)."
  value       = "https://${local.primary_domain}/${var.image_prefix}/"
}

