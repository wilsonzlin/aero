import type { Config } from "../src/config.js";

export function makeTestConfig(overrides: Partial<Config> = {}): Config {
  return {
    HOST: "127.0.0.1",
    PORT: 0,
    LOG_LEVEL: "silent",
    ALLOWED_ORIGINS: ["http://localhost"],
    PUBLIC_BASE_URL: "http://localhost",
    SHUTDOWN_GRACE_MS: 100,
    CROSS_ORIGIN_ISOLATION: false,
    TRUST_PROXY: false,

    SESSION_SECRET: "test-secret",
    SESSION_TTL_SECONDS: 60 * 60 * 24,
    SESSION_COOKIE_SAMESITE: "Lax",

    RATE_LIMIT_REQUESTS_PER_MINUTE: 0,

    TLS_ENABLED: false,
    TLS_CERT_PATH: "",
    TLS_KEY_PATH: "",

    TCP_ALLOW_PRIVATE_IPS: true,
    TCP_ALLOWED_HOSTS: [],
    TCP_ALLOWED_PORTS: [],
    TCP_BLOCKED_CLIENT_IPS: [],
    TCP_MUX_MAX_STREAMS: 1024,
    TCP_MUX_MAX_STREAM_BUFFER_BYTES: 1024 * 1024,
    TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: 16 * 1024 * 1024,
    TCP_PROXY_MAX_CONNECTIONS: 1,
    TCP_PROXY_MAX_CONNECTIONS_PER_IP: 0,
    TCP_PROXY_MAX_MESSAGE_BYTES: 1024 * 1024,
    TCP_PROXY_CONNECT_TIMEOUT_MS: 10_000,
    TCP_PROXY_IDLE_TIMEOUT_MS: 300_000,

    DNS_UPSTREAMS: [],
    DNS_UPSTREAM_TIMEOUT_MS: 200,
    DNS_CACHE_MAX_ENTRIES: 0,
    DNS_CACHE_MAX_TTL_SECONDS: 0,
    DNS_CACHE_NEGATIVE_TTL_SECONDS: 0,
    DNS_MAX_QUERY_BYTES: 4096,
    DNS_MAX_RESPONSE_BYTES: 4096,
    DNS_ALLOW_ANY: true,
    DNS_ALLOW_PRIVATE_PTR: true,
    DNS_QPS_PER_IP: 0,
    DNS_BURST_PER_IP: 0,

    UDP_RELAY_BASE_URL: "",
    UDP_RELAY_AUTH_MODE: "none",
    UDP_RELAY_API_KEY: "",
    UDP_RELAY_JWT_SECRET: "",
    UDP_RELAY_TOKEN_TTL_SECONDS: 300,
    UDP_RELAY_AUDIENCE: "",
    UDP_RELAY_ISSUER: "",

    ...overrides
  };
}

export const TEST_WS_HANDSHAKE_HEADERS: Readonly<Record<string, string>> = {
  upgrade: "websocket",
  connection: "Upgrade",
  "sec-websocket-version": "13",
  // A stable base64 nonce; value content doesn't matter for tests, only that it is non-empty.
  "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ=="
} as const;

