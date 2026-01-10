import { getSignedCookies, getSignedUrl } from "@aws-sdk/cloudfront-signer";

import { ApiError } from "./errors";

export interface SignedCookie {
  name: string;
  value: string;
  attributes: string[];
}

export type StreamAuth =
  | { type: "cookie"; cookies: SignedCookie[]; expiresAt: string }
  | { type: "url"; expiresAt: string }
  | { type: "none" };

function normalizeDomainToBaseUrl(domain: string): string {
  if (domain.startsWith("https://") || domain.startsWith("http://")) return domain;
  return `https://${domain}`;
}

export function buildCloudFrontUrl(params: {
  cloudfrontDomain: string;
  path: string;
}): string {
  const baseUrl = normalizeDomainToBaseUrl(params.cloudfrontDomain).replace(/\/$/, "");
  const path = params.path.startsWith("/") ? params.path : `/${params.path}`;
  return `${baseUrl}${path}`;
}

export function createSignedCookies(params: {
  url: string;
  keyPairId: string;
  privateKeyPem: string;
  expiresAt: Date;
  cookieDomain?: string;
  cookiePath?: string;
}): SignedCookie[] {
  const cookies = getSignedCookies({
    url: params.url,
    keyPairId: params.keyPairId,
    privateKey: params.privateKeyPem,
    dateLessThan: params.expiresAt.toISOString(),
  });

  const baseAttributes: string[] = [
    `Path=${params.cookiePath ?? "/"}`,
    "Secure",
    "SameSite=None",
  ];
  if (params.cookieDomain) baseAttributes.push(`Domain=${params.cookieDomain}`);

  // Use an explicit Expires attribute so browsers drop the cookie when the CloudFront policy expires.
  baseAttributes.push(`Expires=${params.expiresAt.toUTCString()}`);

  return Object.entries(cookies).map(([name, value]) => ({
    name,
    value,
    attributes: [...baseAttributes],
  }));
}

export function formatSetCookie(cookie: SignedCookie): string {
  return `${cookie.name}=${cookie.value}; ${cookie.attributes.join("; ")}`;
}

export function createSignedUrl(params: {
  url: string;
  keyPairId: string;
  privateKeyPem: string;
  expiresAt: Date;
}): string {
  return getSignedUrl({
    url: params.url,
    keyPairId: params.keyPairId,
    privateKey: params.privateKeyPem,
    dateLessThan: params.expiresAt.toISOString(),
  });
}

export function assertCloudFrontSigningConfigured(params: {
  cloudfrontDomain?: string;
  cloudfrontKeyPairId?: string;
  cloudfrontPrivateKeyPem?: string;
}): asserts params is {
  cloudfrontDomain: string;
  cloudfrontKeyPairId: string;
  cloudfrontPrivateKeyPem: string;
} {
  if (!params.cloudfrontDomain) {
    throw new ApiError(500, "CLOUDFRONT_DOMAIN is not configured", "MISCONFIG");
  }
  if (!params.cloudfrontKeyPairId) {
    throw new ApiError(
      500,
      "CLOUDFRONT_KEY_PAIR_ID is not configured",
      "MISCONFIG"
    );
  }
  if (!params.cloudfrontPrivateKeyPem) {
    throw new ApiError(
      500,
      "CLOUDFRONT_PRIVATE_KEY_PEM is not configured",
      "MISCONFIG"
    );
  }
}

export function assertCloudFrontSigningConfiguredForConfig<
  T extends {
    cloudfrontDomain?: string;
    cloudfrontKeyPairId?: string;
    cloudfrontPrivateKeyPem?: string;
  },
>(
  config: T
): asserts config is T & {
  cloudfrontDomain: string;
  cloudfrontKeyPairId: string;
  cloudfrontPrivateKeyPem: string;
} {
  assertCloudFrontSigningConfigured(config);
}
