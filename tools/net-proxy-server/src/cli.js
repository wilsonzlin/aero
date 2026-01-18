import { createProxyServer } from "./server.js";

function envBool(name, defaultValue) {
  const raw = process.env[name];
  if (raw == null) return defaultValue;
  return raw === "1" || raw.toLowerCase() === "true";
}

function envInt(name, defaultValue) {
  const raw = process.env[name];
  if (raw == null) return defaultValue;
  const n = Number(raw);
  return Number.isFinite(n) ? n : defaultValue;
}

const host = process.env.AERO_PROXY_HOST ?? "127.0.0.1";
const port = envInt("AERO_PROXY_PORT", 8080);
const authToken = process.env.AERO_PROXY_AUTH_TOKEN;
const allowPrivateIps = envBool("AERO_PROXY_ALLOW_PRIVATE_IPS", false);
const allowCidrsRaw = process.env.AERO_PROXY_ALLOW_CIDRS ?? "";
const allowCidrs = allowCidrsRaw
  .split(",")
  .map((s) => s.trim())
  .filter((s) => s.length > 0);

if (!authToken) {
  // eslint-disable-next-line no-console
  console.error("Missing AERO_PROXY_AUTH_TOKEN");
  process.exit(1);
}

const server = await createProxyServer({
  host,
  port,
  authToken,
  allowPrivateIps,
  allowCidrs,
});

// eslint-disable-next-line no-console
console.log(`Aero net proxy listening on ${server.url}`);

let shuttingDown = false;
process.on("SIGINT", () => {
  if (shuttingDown) return;
  shuttingDown = true;
  void (async () => {
    // eslint-disable-next-line no-console
    console.log("Shutting down...");
    await server.close();
    process.exit(0);
  })().catch(() => {
    process.exit(1);
  });
});
