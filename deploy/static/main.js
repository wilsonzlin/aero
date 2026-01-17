const coi = window.crossOriginIsolated === true;
document.querySelector("#coi").textContent = String(window.crossOriginIsolated);
document.querySelector("#coi").className = coi ? "ok" : "bad";

const sab = typeof SharedArrayBuffer !== "undefined";
document.querySelector("#sab").textContent = typeof SharedArrayBuffer;
document.querySelector("#sab").className = sab ? "ok" : "bad";

document.querySelector("#origin").textContent = location.origin;

function wsCloseSafe(ws) {
  try {
    ws.close();
  } catch {
    // ignore
  }
}

// This smoke page is often served from the same reverse-proxy base path as the
// gateway (e.g. https://example.com/aero/). Avoid hard-coded absolute paths like
// `/session` which would drop the base path prefix.
//
// Note: we intentionally do *not* use `new URL(".", location.href)` here because
// it treats `https://example.com/aero` (no trailing slash) as a "file" URL and
// resolves `"."` to `/`, dropping the `/aero` prefix.
function computeBasePath() {
  // Prefer deriving the base path from the URL of this script itself, because the
  // document can be served at arbitrary client-routed paths (SPA-style rewrites)
  // while the gateway still lives under the static asset prefix.
  try {
    const scriptDir = new URL(".", import.meta.url).pathname.replace(/\/$/, "");
    if (scriptDir !== "" && scriptDir !== "/") return scriptDir;
  } catch {
    // Ignore and fall back to location-based heuristics.
  }

  let pathname = location.pathname;

  // If we were served from a directory URL (`/aero/`), strip the trailing `/`.
  if (pathname.endsWith("/")) pathname = pathname.replace(/\/+$/, "");

  // If we were served from an explicit file URL (`/aero/index.html`), use the
  // dirname (`/aero`).
  const lastSegment = pathname.split("/").pop() ?? "";
  if (!pathname.endsWith("/") && lastSegment.lastIndexOf(".") > 0) {
    pathname = pathname.slice(0, pathname.lastIndexOf("/"));
  }

  // Represent the origin root as an empty prefix, so `${basePath}/healthz`
  // resolves to `/healthz` rather than `//healthz`.
  return pathname === "" || pathname === "/" ? "" : pathname;
}
const basePath = computeBasePath();

const checks = [
  `Secure context: ${window.isSecureContext}`,
  `COI: ${window.crossOriginIsolated}`,
  `SAB: ${typeof SharedArrayBuffer}`,
  "",
  "If COI is false:",
  "- Confirm COOP/COEP/CORP headers are present on the *main document* response.",
  "- Confirm your browser trusts the TLS certificate (esp. when using localhost/self-signed).",
  "- Ensure all subresources are same-origin OR explicitly CORS/CORP-enabled.",
];
document.querySelector("#checks").textContent = checks.join("\n");

// Basic "is the reverse proxy wiring correct?" checks.
try {
  const res = await fetch(`${basePath}/healthz`, { cache: "no-store" });
  const contentType = res.headers.get("content-type") ?? "";
  let ok = false;
  if (res.ok) {
    if (contentType.includes("application/json")) {
      const json = await res.json();
      ok = json?.ok === true;
    } else {
      const text = await res.text();
      ok = text.trim() === "ok";
    }
  }
  const el = document.querySelector("#health");
  el.textContent = ok ? "ok" : `unexpected (${res.status})`;
  el.className = ok ? "ok" : "bad";
} catch (err) {
  const el = document.querySelector("#health");
  el.textContent = "failed";
  el.className = "bad";
}

let sessionOk = false;
let sessionJson = null;
try {
  const res = await fetch(`${basePath}/session`, {
    method: "POST",
    cache: "no-store",
    headers: { "content-type": "application/json" },
    body: "{}",
  });
  sessionJson = await res.json().catch(() => null);
  sessionOk = res.ok && typeof sessionJson?.session?.expiresAt === "string";
  const el = document.querySelector("#session");
  el.textContent = sessionOk ? "ok" : `unexpected (${res.status})`;
  el.className = sessionOk ? "ok" : "bad";

  const l2LimitsEl = document.querySelector("#session-l2-limits");
  const maxFramePayloadBytes = sessionJson?.limits?.l2?.maxFramePayloadBytes;
  const maxControlPayloadBytes = sessionJson?.limits?.l2?.maxControlPayloadBytes;
  if (typeof maxFramePayloadBytes === "number" && typeof maxControlPayloadBytes === "number") {
    l2LimitsEl.textContent = `frame=${maxFramePayloadBytes} control=${maxControlPayloadBytes}`;
    l2LimitsEl.className = "ok";
  } else {
    l2LimitsEl.textContent = sessionOk ? "missing" : "n/a";
    l2LimitsEl.className = sessionOk ? "bad" : "";
  }
} catch (err) {
  const el = document.querySelector("#session");
  el.textContent = "failed";
  el.className = "bad";

  const l2LimitsEl = document.querySelector("#session-l2-limits");
  l2LimitsEl.textContent = "n/a";
  l2LimitsEl.className = "";
}

try {
  const wsProto = location.protocol === "https:" ? "wss:" : "ws:";
  const tcpPath = sessionJson?.endpoints?.tcp ?? `${basePath}/tcp`;
  const wsUrl = new URL(tcpPath, `${wsProto}//${location.host}`);
  // aero-gateway requires a target host+port for /tcp (v=1 protocol).
  // We use the canonical `host` + `port` form here.
  //
  // We use a public host so the default deployment does not need to opt in to
  // allowing private IPs (unsafe in real production).
  wsUrl.searchParams.set("v", "1");
  wsUrl.searchParams.set("host", "example.com");
  wsUrl.searchParams.set("port", "80");
  const wsEl = document.querySelector("#ws");
  if (!sessionOk) {
    wsEl.textContent = "skipped (no session)";
    wsEl.className = "bad";
  } else {
    const ws = new WebSocket(wsUrl.toString());
    const timeout = setTimeout(() => {
      wsEl.textContent = "timeout";
      wsEl.className = "bad";
      wsCloseSafe(ws);
    }, 5000);

    ws.onopen = () => {
      clearTimeout(timeout);
      wsEl.textContent = "ok";
      wsEl.className = "ok";
      wsCloseSafe(ws);
    };

    ws.onerror = () => {
      clearTimeout(timeout);
      wsEl.textContent = "failed";
      wsEl.className = "bad";
    };
  }
} catch (err) {
  const el = document.querySelector("#ws");
  el.textContent = "failed";
  el.className = "bad";
}

try {
  const wsProto = location.protocol === "https:" ? "wss:" : "ws:";
  const l2Path = sessionJson?.endpoints?.l2 ?? `${basePath}/l2`;
  const wsUrl = new URL(l2Path, `${wsProto}//${location.host}`);
  const wsEl = document.querySelector("#ws-l2");
  // `/l2` auth is deployment-dependent:
  // - Canonical compose (`deploy/docker-compose.yml`) defaults to `AERO_L2_AUTH_MODE=none`, so
  //   `/l2` should succeed even if `POST /session` failed.
  // - If your deployment uses session-cookie auth (`AERO_L2_AUTH_MODE=session`; legacy alias: `cookie`),
  //   `POST /session` is required to mint the `aero_session` cookie.
  const noSessionSuffix = sessionOk ? "" : " (no session)";

  // Prefer URL fragment params (`#l2Token=...`) over query params (`?l2Token=...`) so secrets
  // don't end up in server logs by default.
  const fragmentParams = new URLSearchParams(location.hash.slice(1));
  const queryParams = new URLSearchParams(location.search);
  const l2Token = fragmentParams.get("l2Token") ?? queryParams.get("l2Token");
  const l2TokenTransportRaw = fragmentParams.get("l2TokenTransport") ?? queryParams.get("l2TokenTransport");
  const l2TokenTransport =
    l2TokenTransportRaw === "query" || l2TokenTransportRaw === "subprotocol" || l2TokenTransportRaw === "both"
      ? l2TokenTransportRaw
      : "subprotocol";

  // Optional: allow the smoke test page to validate token-authenticated `/l2`
  // deployments by visiting e.g.:
  //   https://localhost/#l2Token=sekrit&l2TokenTransport=subprotocol
  //
  // If not set, the default compose stack still succeeds (no token required unless `/l2`
  // is configured with token/JWT auth).
  let protocols = "aero-l2-tunnel-v1";
  if (l2Token) {
    if (l2TokenTransport === "query" || l2TokenTransport === "both") {
      wsUrl.searchParams.set("token", l2Token);
    }
    if (l2TokenTransport === "subprotocol" || l2TokenTransport === "both") {
      protocols = ["aero-l2-tunnel-v1", `aero-l2-token.${l2Token}`];
    }
  }

  const ws = new WebSocket(wsUrl.toString(), protocols);
  const timeout = setTimeout(() => {
    wsEl.textContent = `timeout${noSessionSuffix}`;
    wsEl.className = "bad";
    wsCloseSafe(ws);
  }, 5000);

  ws.onopen = () => {
    clearTimeout(timeout);
    wsEl.textContent = `ok${noSessionSuffix}`;
    wsEl.className = "ok";
    wsCloseSafe(ws);
  };

  ws.onerror = () => {
    clearTimeout(timeout);
    wsEl.textContent = `failed${noSessionSuffix}`;
    wsEl.className = "bad";
  };
} catch (err) {
  const el = document.querySelector("#ws-l2");
  el.textContent = "failed";
  el.className = "bad";
}
