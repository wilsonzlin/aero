const coi = window.crossOriginIsolated === true;
document.querySelector("#coi").textContent = String(window.crossOriginIsolated);
document.querySelector("#coi").className = coi ? "ok" : "bad";

const sab = typeof SharedArrayBuffer !== "undefined";
document.querySelector("#sab").textContent = typeof SharedArrayBuffer;
document.querySelector("#sab").className = sab ? "ok" : "bad";

document.querySelector("#origin").textContent = location.origin;

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
  const res = await fetch("/healthz", { cache: "no-store" });
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

try {
  const wsProto = location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = new URL("/tcp", `${wsProto}//${location.host}`);
  // aero-gateway requires a target host+port for /tcp (v=1 protocol).
  // We use the canonical `host` + `port` form here.
  //
  // We use a public host so the default deployment does not need to opt in to
  // allowing private IPs (unsafe in real production).
  wsUrl.searchParams.set("v", "1");
  wsUrl.searchParams.set("host", "example.com");
  wsUrl.searchParams.set("port", "80");
  const wsEl = document.querySelector("#ws");
  const ws = new WebSocket(wsUrl.toString());
  const timeout = setTimeout(() => {
    wsEl.textContent = "timeout";
    wsEl.className = "bad";
    ws.close();
  }, 5000);

  ws.onopen = () => {
    clearTimeout(timeout);
    wsEl.textContent = "ok";
    wsEl.className = "ok";
    ws.close();
  };

  ws.onerror = () => {
    clearTimeout(timeout);
    wsEl.textContent = "failed";
    wsEl.className = "bad";
  };
} catch (err) {
  const el = document.querySelector("#ws");
  el.textContent = "failed";
  el.className = "bad";
}
