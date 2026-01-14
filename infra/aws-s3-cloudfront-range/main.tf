data "aws_caller_identity" "current" {}

locals {
  # CloudFront policy names only allow alphanumerics, hyphens, and underscores.
  name_prefix = substr(replace(var.bucket_name, "/[^0-9A-Za-z_-]/", "-"), 0, 64)
  origin_id   = "${local.name_prefix}-s3-origin"

  # CloudFront path patterns are specified without a leading "/" (for example: "images/*"),
  # but they apply to request paths like "/images/...".
  image_path_pattern = "${var.image_prefix}/*"
  image_uri_prefix   = "/${var.image_prefix}/"

  cache_policy_id = var.cache_policy_mode == "immutable" ? aws_cloudfront_cache_policy.immutable.id : aws_cloudfront_cache_policy.mutable.id
  primary_domain  = length(var.custom_domain_names) > 0 ? var.custom_domain_names[0] : aws_cloudfront_distribution.images.domain_name

  # Attach an edge response headers policy when we need to inject/override headers.
  # This is used for both CORS headers (enable_edge_cors) and optional security headers
  # like Cross-Origin-Resource-Policy (cross_origin_resource_policy).
  enable_edge_response_headers_policy = var.enable_edge_cors || var.cross_origin_resource_policy != null

  # Origin request headers:
  # - CORS preflight (OPTIONS) needs these forwarded to S3 unless you enable the optional
  #   edge-handled preflight function (enable_edge_cors_preflight).
  # - Forward Range headers to S3 for efficient partial responses. CloudFront can cache byte
  #   ranges without including Range in the cache key (avoids cache fragmentation).
  #   See docs/17-range-cdn-behavior.md.
  origin_request_headers = [
    "Origin",
    "Access-Control-Request-Method",
    "Access-Control-Request-Headers",
    "Range",
    "If-Range",
    # Conditional request headers. CloudFront can handle conditional requests at the edge, but
    # forwarding these to the origin improves portability when you rely on origin validators.
    "If-None-Match",
    "If-Modified-Since",
  ]
}

resource "aws_s3_bucket" "images" {
  bucket        = var.bucket_name
  force_destroy = var.force_destroy
  tags          = var.tags
}

resource "aws_s3_bucket_public_access_block" "images" {
  bucket = aws_s3_bucket.images.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "images" {
  bucket = aws_s3_bucket.images.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = var.kms_key_arn == null ? "AES256" : "aws:kms"
      kms_master_key_id = var.kms_key_arn
    }
  }
}

resource "aws_s3_bucket_versioning" "images" {
  bucket = aws_s3_bucket.images.id

  versioning_configuration {
    status = var.enable_versioning ? "Enabled" : "Suspended"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "images" {
  bucket = aws_s3_bucket.images.id

  rule {
    id     = "disk-images"
    status = "Enabled"

    filter {
      prefix = "${var.image_prefix}/"
    }

    abort_incomplete_multipart_upload {
      days_after_initiation = var.abort_incomplete_multipart_upload_days
    }

    dynamic "transition" {
      for_each = var.lifecycle_transition_days == null ? [] : [1]
      content {
        days          = var.lifecycle_transition_days
        storage_class = var.lifecycle_transition_storage_class
      }
    }

    dynamic "expiration" {
      for_each = var.lifecycle_expiration_days == null ? [] : [1]
      content {
        days = var.lifecycle_expiration_days
      }
    }

    dynamic "noncurrent_version_expiration" {
      for_each = var.noncurrent_version_expiration_days == null ? [] : [1]
      content {
        noncurrent_days = var.noncurrent_version_expiration_days
      }
    }
  }
}

resource "aws_s3_bucket_cors_configuration" "images" {
  count  = var.enable_s3_cors && length(var.cors_allowed_origins) > 0 ? 1 : 0
  bucket = aws_s3_bucket.images.id

  cors_rule {
    allowed_methods = var.cors_allowed_methods
    allowed_origins = var.cors_allowed_origins
    allowed_headers = var.cors_allowed_headers
    expose_headers  = var.cors_expose_headers
    max_age_seconds = var.cors_max_age_seconds
  }
}

resource "aws_cloudfront_origin_access_control" "images" {
  name                              = "${local.name_prefix}-oac"
  description                       = "OAC for ${var.bucket_name}"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

resource "aws_cloudfront_cache_policy" "immutable" {
  name        = "${local.name_prefix}-images-immutable"
  comment     = "Disk images: long TTL for versioned/immutable keys"
  default_ttl = var.immutable_default_ttl_seconds
  max_ttl     = var.immutable_max_ttl_seconds
  min_ttl     = var.immutable_min_ttl_seconds

  parameters_in_cache_key_and_forwarded_to_origin {
    cookies_config {
      cookie_behavior = "none"
    }

    headers_config {
      header_behavior = "none"
    }

    query_strings_config {
      query_string_behavior = "none"
    }

    enable_accept_encoding_brotli = false
    enable_accept_encoding_gzip   = false
  }
}

resource "aws_cloudfront_cache_policy" "mutable" {
  name        = "${local.name_prefix}-images-mutable"
  comment     = "Disk images: short TTL for mutable keys"
  default_ttl = var.mutable_default_ttl_seconds
  max_ttl     = var.mutable_max_ttl_seconds
  min_ttl     = var.mutable_min_ttl_seconds

  parameters_in_cache_key_and_forwarded_to_origin {
    cookies_config {
      cookie_behavior = "none"
    }

    headers_config {
      header_behavior = "none"
    }

    query_strings_config {
      query_string_behavior = "none"
    }

    enable_accept_encoding_brotli = false
    enable_accept_encoding_gzip   = false
  }
}

resource "aws_cloudfront_origin_request_policy" "images" {
  name    = "${local.name_prefix}-images-cors-range"
  comment = "Forward only headers required for CORS preflight and Range requests"

  cookies_config {
    cookie_behavior = "none"
  }

  headers_config {
    header_behavior = "whitelist"
    headers {
      items = local.origin_request_headers
    }
  }

  query_strings_config {
    query_string_behavior = "none"
  }
}

resource "aws_cloudfront_response_headers_policy" "cors" {
  count = local.enable_edge_response_headers_policy ? 1 : 0

  name    = "${local.name_prefix}-images-cors"
  comment = "Optional: add/override CORS and security headers at CloudFront edge"

  dynamic "cors_config" {
    for_each = var.enable_edge_cors ? [1] : []
    content {
      access_control_allow_credentials = var.cors_allow_credentials
      access_control_max_age_sec       = var.cors_max_age_seconds
      origin_override                  = true

      access_control_allow_headers {
        items = var.cors_allowed_headers
      }

      access_control_allow_methods {
        items = var.cors_allowed_methods
      }

      access_control_allow_origins {
        items = var.cors_allowed_origins
      }

      access_control_expose_headers {
        items = var.cors_expose_headers
      }
    }
  }

  dynamic "custom_headers_config" {
    for_each = var.cross_origin_resource_policy == null ? [] : [1]
    content {
      items {
        header   = "Cross-Origin-Resource-Policy"
        value    = var.cross_origin_resource_policy
        override = false
      }
    }
  }
}

resource "aws_cloudfront_function" "cors_preflight" {
  count   = var.enable_edge_cors_preflight ? 1 : 0
  name    = "${local.name_prefix}-cors-preflight"
  runtime = "cloudfront-js-1.0"
  comment = "Edge-handled CORS preflight for OPTIONS ${local.image_uri_prefix}*"
  publish = true

  code = templatefile("${path.module}/cors-preflight.js.tftpl", {
    allowed_origins_json  = jsonencode(var.cors_allowed_origins)
    allowed_headers_json  = jsonencode(var.cors_allowed_headers)
    allow_credentials     = var.cors_allow_credentials
    max_age_seconds       = var.cors_max_age_seconds
    image_uri_prefix_json = jsonencode(local.image_uri_prefix)
    corp_json             = jsonencode(var.cross_origin_resource_policy)
  })
}

resource "aws_cloudfront_distribution" "images" {
  enabled         = true
  is_ipv6_enabled = true
  comment         = "Disk images for ${var.bucket_name}"
  price_class     = var.cloudfront_price_class
  http_version    = "http2and3"
  aliases         = var.custom_domain_names
  tags            = var.tags

  origin {
    domain_name              = aws_s3_bucket.images.bucket_regional_domain_name
    origin_id                = local.origin_id
    origin_access_control_id = aws_cloudfront_origin_access_control.images.id

    s3_origin_config {
      origin_access_identity = ""
    }
  }

  # A CloudFront distribution always requires a default cache behavior. We keep it aligned with
  # the /<image_prefix>/* behavior; requests outside that prefix will typically be denied by the
  # bucket policy anyway.
  default_cache_behavior {
    target_origin_id       = local.origin_id
    viewer_protocol_policy = "redirect-to-https"

    allowed_methods = ["GET", "HEAD", "OPTIONS"]
    cached_methods  = ["GET", "HEAD"]

    compress = false

    cache_policy_id            = local.cache_policy_id
    origin_request_policy_id   = aws_cloudfront_origin_request_policy.images.id
    response_headers_policy_id = local.enable_edge_response_headers_policy ? aws_cloudfront_response_headers_policy.cors[0].id : null

    dynamic "function_association" {
      for_each = var.enable_edge_cors_preflight ? [1] : []
      content {
        event_type   = "viewer-request"
        function_arn = aws_cloudfront_function.cors_preflight[0].arn
      }
    }
  }

  ordered_cache_behavior {
    path_pattern           = local.image_path_pattern
    target_origin_id       = local.origin_id
    viewer_protocol_policy = "redirect-to-https"

    allowed_methods = ["GET", "HEAD", "OPTIONS"]
    cached_methods  = ["GET", "HEAD"]

    compress = false

    cache_policy_id            = local.cache_policy_id
    origin_request_policy_id   = aws_cloudfront_origin_request_policy.images.id
    response_headers_policy_id = local.enable_edge_response_headers_policy ? aws_cloudfront_response_headers_policy.cors[0].id : null

    dynamic "function_association" {
      for_each = var.enable_edge_cors_preflight ? [1] : []
      content {
        event_type   = "viewer-request"
        function_arn = aws_cloudfront_function.cors_preflight[0].arn
      }
    }
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    cloudfront_default_certificate = length(var.custom_domain_names) == 0

    acm_certificate_arn      = length(var.custom_domain_names) == 0 ? null : var.acm_certificate_arn
    ssl_support_method       = length(var.custom_domain_names) == 0 ? null : "sni-only"
    minimum_protocol_version = length(var.custom_domain_names) == 0 ? null : "TLSv1.2_2021"
  }
}

data "aws_iam_policy_document" "bucket_policy" {
  statement {
    sid     = "AllowCloudFrontReadOnly"
    effect  = "Allow"
    actions = ["s3:GetObject"]

    resources = [
      "${aws_s3_bucket.images.arn}/${var.image_prefix}/*",
    ]

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.images.arn]
    }

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceAccount"
      values   = [data.aws_caller_identity.current.account_id]
    }
  }
}

resource "aws_s3_bucket_policy" "images" {
  bucket = aws_s3_bucket.images.id
  policy = data.aws_iam_policy_document.bucket_policy.json
}
