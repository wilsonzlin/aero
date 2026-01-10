variable "aws_region" {
  description = "AWS region for the S3 bucket (CloudFront is global, but is still managed via this provider)."
  type        = string
}

variable "bucket_name" {
  description = "Globally-unique S3 bucket name to store disk images."
  type        = string
}

variable "image_prefix" {
  description = "S3 key prefix (and CloudFront path prefix) under which images are stored. Default matches /images/*."
  type        = string
  default     = "images"

  validation {
    condition     = length(var.image_prefix) > 0 && !startswith(var.image_prefix, "/")
    error_message = "image_prefix must be non-empty and must not start with '/'."
  }
}

variable "force_destroy" {
  description = "Whether to allow Terraform to destroy the bucket even if it contains objects."
  type        = bool
  default     = false
}

variable "enable_versioning" {
  description = "Enable S3 bucket versioning."
  type        = bool
  default     = false
}

variable "kms_key_arn" {
  description = "Optional KMS key ARN for SSE-KMS. If null, SSE-S3 (AES256) is used."
  type        = string
  default     = null
}

variable "abort_incomplete_multipart_upload_days" {
  description = "Abort incomplete multipart uploads after this many days."
  type        = number
  default     = 7
}

variable "lifecycle_transition_days" {
  description = "Optional: transition objects to another storage class after this many days. Set to null to disable."
  type        = number
  default     = null
}

variable "lifecycle_transition_storage_class" {
  description = "Storage class to transition objects to (e.g. STANDARD_IA, INTELLIGENT_TIERING)."
  type        = string
  default     = "STANDARD_IA"
}

variable "lifecycle_expiration_days" {
  description = "Optional: expire current objects after this many days. Set to null to disable."
  type        = number
  default     = null
}

variable "noncurrent_version_expiration_days" {
  description = "Optional: expire noncurrent object versions after this many days (only relevant if versioning is enabled). Set to null to disable."
  type        = number
  default     = null
}

variable "cors_allowed_origins" {
  description = "Allowed origins for CORS. If empty, no S3 CORS configuration is applied (same-origin only)."
  type        = list(string)
  default     = []
}

variable "cors_allowed_methods" {
  description = "Allowed CORS methods for disk images."
  type        = list(string)
  default     = ["GET", "HEAD", "OPTIONS"]
}

variable "cors_allowed_headers" {
  description = "Allowed CORS request headers. Must include Range for HTTP Range requests from browsers."
  type        = list(string)
  default = [
    "Range",
    "Origin",
    "Access-Control-Request-Method",
    "Access-Control-Request-Headers",
  ]
}

variable "cors_expose_headers" {
  description = "CORS response headers to expose to browsers (so clients can read Content-Range, etc)."
  type        = list(string)
  default = [
    "Accept-Ranges",
    "Content-Length",
    "Content-Range",
    "ETag",
  ]
}

variable "cors_max_age_seconds" {
  description = "How long browsers can cache the CORS preflight response."
  type        = number
  default     = 600
}

variable "enable_edge_cors" {
  description = "If true, attach a CloudFront response headers policy to inject/override CORS headers at the edge."
  type        = bool
  default     = false

  validation {
    condition     = !var.enable_edge_cors || length(var.cors_allowed_origins) > 0
    error_message = "enable_edge_cors=true requires cors_allowed_origins to be non-empty."
  }
}

variable "cache_policy_mode" {
  description = "Select which CloudFront cache policy to use: immutable (long TTL) or mutable (short TTL)."
  type        = string
  default     = "immutable"

  validation {
    condition     = contains(["immutable", "mutable"], var.cache_policy_mode)
    error_message = "cache_policy_mode must be either \"immutable\" or \"mutable\"."
  }
}

variable "immutable_min_ttl_seconds" {
  description = "CloudFront min TTL for immutable cache policy."
  type        = number
  default     = 0
}

variable "immutable_default_ttl_seconds" {
  description = "CloudFront default TTL for immutable cache policy."
  type        = number
  default     = 31536000
}

variable "immutable_max_ttl_seconds" {
  description = "CloudFront max TTL for immutable cache policy."
  type        = number
  default     = 31536000
}

variable "mutable_min_ttl_seconds" {
  description = "CloudFront min TTL for mutable cache policy."
  type        = number
  default     = 0
}

variable "mutable_default_ttl_seconds" {
  description = "CloudFront default TTL for mutable cache policy."
  type        = number
  default     = 300
}

variable "mutable_max_ttl_seconds" {
  description = "CloudFront max TTL for mutable cache policy."
  type        = number
  default     = 3600
}

variable "cloudfront_price_class" {
  description = "CloudFront price class (e.g. PriceClass_All, PriceClass_200, PriceClass_100)."
  type        = string
  default     = "PriceClass_All"
}

variable "custom_domain_names" {
  description = "Optional custom domain names (CNAMEs) for the CloudFront distribution."
  type        = list(string)
  default     = []
}

variable "acm_certificate_arn" {
  description = "ACM certificate ARN (must be in us-east-1) for custom_domain_names. Required if custom_domain_names is non-empty."
  type        = string
  default     = null

  validation {
    condition     = length(var.custom_domain_names) == 0 || var.acm_certificate_arn != null
    error_message = "acm_certificate_arn must be set when custom_domain_names is non-empty."
  }
}

variable "tags" {
  description = "Tags to apply to created resources."
  type        = map(string)
  default     = {}
}
