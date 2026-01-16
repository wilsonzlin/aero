import fs from "node:fs";

import type { CookieSameSite } from "./cloudfront";

export type AuthMode = "dev" | "none";
export type CloudFrontAuthMode = "cookie" | "url";
export type CrossOriginResourcePolicy = "same-origin" | "same-site" | "cross-origin";

export type ImageCacheControlMode = "public-immutable" | "private-no-store";

export const CACHE_CONTROL_PUBLIC_IMMUTABLE =
  "public, max-age=31536000, immutable, no-transform";
export const CACHE_CONTROL_PRIVATE_NO_STORE = "private, no-store, no-transform";

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
  cloudfrontCookieSameSite: CookieSameSite;
  cloudfrontCookiePartitioned: boolean;
  cloudfrontSignedTtlSeconds: number;

  imageBasePath: string;
  partSizeBytes: number;
  imageCacheControl: string;

  authMode: AuthMode;
  port: number;
  corsAllowOrigin: string;
  crossOriginResourcePolicy: CrossOriginResourcePolicy;
}

const MAX_ENV_VALUE_LEN = 4 * 1024;
const MAX_DOMAIN_LEN = 1024;
const MAX_BASE_PATH_LEN = 1024;

function formatForError(value: string, maxLen = 128): string {
  if (maxLen <= 0) return `(${value.length} chars)`;
  if (value.length <= maxLen) return value;
  return `${value.slice(0, maxLen)}…(${value.length} chars)`;
}

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required env var: ${name}`);
  }
  return value;
}

function assertNoControlChars(name: string, value: string): void {
  for (let i = 0; i < value.length; i++) {
    const c = value.charCodeAt(i);
    if (c <= 0x1f || c === 0x7f) {
      throw new Error(`Invalid ${name}: contains control characters`);
    }
  }
}

function normalizeCorsAllowOrigin(raw: string): string {
  const value = raw.trim();
  if (!value) throw new Error("Invalid CORS_ALLOW_ORIGIN: empty");
  if (value.length > MAX_ENV_VALUE_LEN) throw new Error("Invalid CORS_ALLOW_ORIGIN: too long");
  assertNoControlChars("CORS_ALLOW_ORIGIN", value);
  if (value === "*") return "*";

  let u: URL;
  try {
    u = new URL(value);
  } catch {
    throw new Error("Invalid CORS_ALLOW_ORIGIN: expected '*' or an http(s) origin");
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") {
    throw new Error("Invalid CORS_ALLOW_ORIGIN: expected http(s) origin");
  }
  if (u.username || u.password) throw new Error("Invalid CORS_ALLOW_ORIGIN: credentials not allowed");
  if (u.pathname !== "/" || u.search || u.hash) {
    throw new Error("Invalid CORS_ALLOW_ORIGIN: must be an origin (no path/query/hash)");
  }
  return u.origin;
}

function normalizeBaseUrlFromDomain(name: string, raw: string | undefined): string | undefined {
  const value = raw?.trim();
  if (!value) return undefined;
  if (value.length > MAX_DOMAIN_LEN) throw new Error(`Invalid ${name}: too long`);
  assertNoControlChars(name, value);

  const withScheme =
    value.startsWith("https://") || value.startsWith("http://") ? value : `https://${value}`;
  let u: URL;
  try {
    u = new URL(withScheme);
  } catch {
    throw new Error(`Invalid ${name}: expected a hostname or origin`);
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") {
    throw new Error(`Invalid ${name}: expected http(s)`);
  }
  if (!u.host) throw new Error(`Invalid ${name}: missing host`);
  if (u.pathname !== "/" || u.search || u.hash) {
    throw new Error(`Invalid ${name}: must be a bare origin/host`);
  }
  return u.origin;
}

function normalizeCookieDomain(raw: string | undefined): string | undefined {
  const value = raw?.trim();
  if (!value) return undefined;
  if (value.length > MAX_DOMAIN_LEN) throw new Error("Invalid CLOUDFRONT_COOKIE_DOMAIN: too long");
  assertNoControlChars("CLOUDFRONT_COOKIE_DOMAIN", value);
  for (let i = 0; i < value.length; i++) {
    const c = value.charCodeAt(i);
    if (c <= 0x20 || c === 0x3b || c === 0x2c) {
      throw new Error("Invalid CLOUDFRONT_COOKIE_DOMAIN");
    }
  }
  return value;
}

function parseBool(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) return fallback;
  if (value === "true") return true;
  if (value === "false") return false;
  throw new Error(`Invalid boolean (got ${formatForError(String(value))})`);
}

function parseIntEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`Invalid ${name}`);
  }
  return value;
}

function parseSameSiteEnv(name: string, fallback: CookieSameSite): CookieSameSite {
  const raw = process.env[name];
  if (!raw) return fallback;
  const normalized = raw.trim().toLowerCase();
  if (normalized === "none") return "None";
  if (normalized === "lax") return "Lax";
  if (normalized === "strict") return "Strict";
  throw new Error(`Invalid ${name}`);
}

function parseCrossOriginResourcePolicy(
  raw: string | undefined,
  fallback: CrossOriginResourcePolicy
): CrossOriginResourcePolicy {
  const trimmed = raw?.trim();
  if (!trimmed) return fallback;
  if (trimmed === "same-origin" || trimmed === "same-site" || trimmed === "cross-origin") {
    return trimmed;
  }
  throw new Error("Invalid CROSS_ORIGIN_RESOURCE_POLICY (expected same-origin, same-site, or cross-origin)");
}

function parseCacheControlMode(value: string | undefined): ImageCacheControlMode {
  const normalized = (value ?? "private-no-store").trim();
  if (
    normalized === "public-immutable" ||
    normalized === "CACHE_CONTROL_PUBLIC_IMMUTABLE"
  ) {
    return "public-immutable";
  }
  if (
    normalized === "private-no-store" ||
    normalized === "CACHE_CONTROL_PRIVATE_NO_STORE"
  ) {
    return "private-no-store";
  }
  throw new Error(
    `Invalid IMAGE_CACHE_CONTROL (got ${formatForError(String(value ?? ""))})`,
  );
}

function cacheControlForMode(mode: ImageCacheControlMode): string {
  if (mode === "public-immutable") return CACHE_CONTROL_PUBLIC_IMMUTABLE;
  return CACHE_CONTROL_PRIVATE_NO_STORE;
}

function normalizeBasePath(basePath: string): string {
  let value = basePath.trim();
  if (!value.startsWith("/")) value = `/${value}`;
  if (value.length > 1 && value.endsWith("/")) value = value.slice(0, -1);
  if (value.length > MAX_BASE_PATH_LEN) throw new Error("Invalid IMAGE_BASE_PATH: too long");
  if (value.includes("\0")) throw new Error("Invalid IMAGE_BASE_PATH");
  // Avoid accidentally generating absolute URLs or query-bearing paths.
  if (value.includes("?") || value.includes("#")) throw new Error("Invalid IMAGE_BASE_PATH");
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
    throw new Error("Invalid AUTH_MODE");
  }

  const cloudfrontAuthMode = (process.env.CLOUDFRONT_AUTH_MODE ??
    "cookie") as CloudFrontAuthMode;
  if (cloudfrontAuthMode !== "cookie" && cloudfrontAuthMode !== "url") {
    throw new Error(
      "Invalid CLOUDFRONT_AUTH_MODE"
    );
  }

  const cloudfrontPrivateKeyRaw = process.env.CLOUDFRONT_PRIVATE_KEY_PEM;
  const cloudfrontPrivateKeyPem = cloudfrontPrivateKeyRaw
    ? loadPem(cloudfrontPrivateKeyRaw)
    : undefined;

  const cloudfrontCookieSameSite = parseSameSiteEnv("CLOUDFRONT_COOKIE_SAMESITE", "None");
  const cloudfrontCookiePartitioned = parseBool(
    process.env.CLOUDFRONT_COOKIE_PARTITIONED,
    false
  );
  if (cloudfrontCookiePartitioned && cloudfrontCookieSameSite !== "None") {
    throw new Error("CLOUDFRONT_COOKIE_PARTITIONED requires CLOUDFRONT_COOKIE_SAMESITE=None");
  }

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

  const port = parseIntEnv("PORT", 3000);
  if (port > 65535) throw new Error(`Invalid PORT: ${port}`);

  const cloudfrontDomain = normalizeBaseUrlFromDomain(
    "CLOUDFRONT_DOMAIN",
    process.env.CLOUDFRONT_DOMAIN
  );
  const cloudfrontCookieDomain = normalizeCookieDomain(process.env.CLOUDFRONT_COOKIE_DOMAIN);

  return {
    s3Bucket: requireEnv("S3_BUCKET"),
    awsRegion: requireEnv("AWS_REGION"),
    s3Endpoint: process.env.S3_ENDPOINT,
    s3ForcePathStyle: parseBool(process.env.S3_FORCE_PATH_STYLE, false),

    cloudfrontDomain,
    cloudfrontKeyPairId: process.env.CLOUDFRONT_KEY_PAIR_ID,
    cloudfrontPrivateKeyPem,
    cloudfrontAuthMode,
    cloudfrontCookieDomain,
    cloudfrontCookieSameSite,
    cloudfrontCookiePartitioned,
    cloudfrontSignedTtlSeconds: parseIntEnv(
      "CLOUDFRONT_SIGNED_TTL_SECONDS",
      60 * 60 * 12
    ),

    imageBasePath: normalizeBasePath(process.env.IMAGE_BASE_PATH ?? "/images"),
    // 64MiB: a reasonable default for large disk images (and well above S3's 5MiB minimum).
    partSizeBytes,
    imageCacheControl: cacheControlForMode(
      parseCacheControlMode(process.env.IMAGE_CACHE_CONTROL)
    ),

    authMode,
    port,
    corsAllowOrigin: normalizeCorsAllowOrigin(process.env.CORS_ALLOW_ORIGIN ?? "*"),
    crossOriginResourcePolicy: parseCrossOriginResourcePolicy(
      process.env.CROSS_ORIGIN_RESOURCE_POLICY,
      "same-site"
    ),
  };
}

export function imageBasePathToPrefix(imageBasePath: string): string {
  // "/images" -> "images"
  return imageBasePath.replace(/^\//, "");
}
