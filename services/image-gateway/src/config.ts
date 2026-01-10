import fs from "node:fs";

export type AuthMode = "dev" | "none";
export type CloudFrontAuthMode = "cookie" | "url";

export interface Config {
  s3Bucket: string;
  awsRegion: string;
  s3Endpoint?: string;
  s3ForcePathStyle: boolean;

  cloudfrontDomain?: string;
  cloudfrontKeyPairId?: string;
  cloudfrontPrivateKeyPem?: string;
  cloudfrontAuthMode: CloudFrontAuthMode;
  cloudfrontCookieDomain?: string;
  cloudfrontSignedTtlSeconds: number;

  imageBasePath: string;
  partSizeBytes: number;

  authMode: AuthMode;
  port: number;
  corsAllowOrigin: string;
}

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required env var: ${name}`);
  }
  return value;
}

function parseBool(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) return fallback;
  if (value === "true") return true;
  if (value === "false") return false;
  throw new Error(`Invalid boolean: ${value}`);
}

function parseIntEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`Invalid ${name}: ${raw}`);
  }
  return value;
}

function normalizeBasePath(basePath: string): string {
  let value = basePath.trim();
  if (!value.startsWith("/")) value = `/${value}`;
  if (value.length > 1 && value.endsWith("/")) value = value.slice(0, -1);
  return value;
}

function loadPem(pemOrPath: string): string {
  const maybeInline = pemOrPath.replace(/\\n/g, "\n");
  if (maybeInline.includes("-----BEGIN")) {
    return maybeInline;
  }

  if (!fs.existsSync(pemOrPath)) {
    throw new Error(
      "CLOUDFRONT_PRIVATE_KEY_PEM did not look like a PEM string and the path does not exist"
    );
  }
  return fs.readFileSync(pemOrPath, "utf8");
}

export function loadConfig(): Config {
  const authMode = (process.env.AUTH_MODE ?? "dev") as AuthMode;
  if (authMode !== "dev" && authMode !== "none") {
    throw new Error(`Invalid AUTH_MODE: ${process.env.AUTH_MODE}`);
  }

  const cloudfrontAuthMode = (process.env.CLOUDFRONT_AUTH_MODE ??
    "cookie") as CloudFrontAuthMode;
  if (cloudfrontAuthMode !== "cookie" && cloudfrontAuthMode !== "url") {
    throw new Error(
      `Invalid CLOUDFRONT_AUTH_MODE: ${process.env.CLOUDFRONT_AUTH_MODE}`
    );
  }

  const cloudfrontPrivateKeyRaw = process.env.CLOUDFRONT_PRIVATE_KEY_PEM;
  const cloudfrontPrivateKeyPem = cloudfrontPrivateKeyRaw
    ? loadPem(cloudfrontPrivateKeyRaw)
    : undefined;

  const partSizeBytes = parseIntEnv(
    "MULTIPART_PART_SIZE_BYTES",
    64 * 1024 * 1024
  );
  // S3 constraints: parts must be at least 5MiB (except the last part), and at most 5GiB.
  if (partSizeBytes < 5 * 1024 * 1024 || partSizeBytes > 5 * 1024 * 1024 * 1024) {
    throw new Error(
      "MULTIPART_PART_SIZE_BYTES must be between 5MiB and 5GiB (inclusive)"
    );
  }

  return {
    s3Bucket: requireEnv("S3_BUCKET"),
    awsRegion: requireEnv("AWS_REGION"),
    s3Endpoint: process.env.S3_ENDPOINT,
    s3ForcePathStyle: parseBool(process.env.S3_FORCE_PATH_STYLE, false),

    cloudfrontDomain: process.env.CLOUDFRONT_DOMAIN,
    cloudfrontKeyPairId: process.env.CLOUDFRONT_KEY_PAIR_ID,
    cloudfrontPrivateKeyPem,
    cloudfrontAuthMode,
    cloudfrontCookieDomain: process.env.CLOUDFRONT_COOKIE_DOMAIN,
    cloudfrontSignedTtlSeconds: parseIntEnv(
      "CLOUDFRONT_SIGNED_TTL_SECONDS",
      60 * 60 * 12
    ),

    imageBasePath: normalizeBasePath(process.env.IMAGE_BASE_PATH ?? "/images"),
    // 64MiB: a reasonable default for large disk images (and well above S3's 5MiB minimum).
    partSizeBytes,

    authMode,
    port: parseIntEnv("PORT", 3000),
    corsAllowOrigin: process.env.CORS_ALLOW_ORIGIN ?? "*",
  };
}

export function imageBasePathToPrefix(imageBasePath: string): string {
  // "/images" -> "images"
  return imageBasePath.replace(/^\//, "");
}
