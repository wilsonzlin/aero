/**
 * Helper for normalizing Aero proxy base URLs into WebSocket endpoint URLs.
 *
 * `proxyUrl` in Aero config can be:
 * - absolute ws(s)://... or http(s)://... URLs
 * - same-origin "/path" URLs (resolved via `globalThis.location.href` when available)
 *
 * This helper:
 * - resolves same-origin paths against `location.href` when present
 * - converts http -> ws and https -> wss
 * - appends an endpoint path segment (e.g. "/udp") without introducing double slashes
 */
export function buildWebSocketUrl(baseUrl: string, endpointPath: string): URL {
  // `proxyUrl` supports same-origin absolute paths (e.g. "/base"). `new URL("/base")`
  // throws unless a base is provided, so resolve against `location.href` when
  // available (browser environments), and fall back to `new URL(baseUrl)` for
  // Node/vitest environments where `location` is typically undefined.
  const url =
    typeof location !== "undefined" && typeof location.href === "string"
      ? new URL(baseUrl, location.href)
      : new URL(baseUrl);
  if (url.protocol === "http:") url.protocol = "ws:";
  if (url.protocol === "https:") url.protocol = "wss:";

  const endpoint = endpointPath.startsWith("/") ? endpointPath : `/${endpointPath}`;
  const path = url.pathname.replace(/\/+$/, "");
  if (path.endsWith(endpoint)) {
    url.pathname = path;
  } else {
    url.pathname = `${path}${endpoint}`;
  }

  return url;
}
