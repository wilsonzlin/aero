import { test, expect } from "@playwright/test";
import * as crypto from "node:crypto";
import dgram from "node:dgram";
import fsSync from "node:fs";
import fs from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import { spawn, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { unrefBestEffort } from "../../../../src/unref_safe.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// v2 UDP relay framing overhead for an IPv6 destination (worst case).
// Keep this in sync with `proxy/webrtc-udp-relay/internal/udpproto.MaxFrameOverheadBytes`.
const UDP_RELAY_V2_IPV6_HEADER_BYTES = 24;
const UDP_RELAY_V2_GUEST_PORT_OFFSET = 4;
const UDP_RELAY_V2_REMOTE_IP_OFFSET = 6;
const UDP_RELAY_V2_IPV6_ADDR_BYTES = 16;
const UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6 = UDP_RELAY_V2_REMOTE_IP_OFFSET + UDP_RELAY_V2_IPV6_ADDR_BYTES;

function base64urlEncode(data) {
  const buf = typeof data === "string" ? Buffer.from(data, "utf8") : Buffer.from(data);
  return buf
    .toString("base64")
    .replaceAll("=", "")
    .replaceAll("+", "-")
    .replaceAll("/", "_");
}

function mintHS256JWT({ sid, iat, exp, secret, extraClaims = {} }) {
  if (!sid) throw new Error("sid is required");
  if (!Number.isFinite(iat) || !Number.isFinite(exp)) throw new Error("iat/exp must be numbers (unix seconds)");
  if (!secret) throw new Error("secret is required");

  const header = { alg: "HS256", typ: "JWT" };
  const payload = { sid, iat, exp, ...extraClaims };
  const signingInput = `${base64urlEncode(JSON.stringify(header))}.${base64urlEncode(JSON.stringify(payload))}`;
  const sig = crypto.createHmac("sha256", secret).update(signingInput).digest();
  return `${signingInput}.${base64urlEncode(sig)}`;
}

function sleep(ms) {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    unrefBestEffort(timeout);
  });
}

function parseRelayEventCounters(metricsText) {
  const out = {};
  for (const rawLine of metricsText.split("\n")) {
    const line = rawLine.trim();
    if (!line.startsWith("aero_webrtc_udp_relay_events_total{event=")) continue;
    const match = /^aero_webrtc_udp_relay_events_total\{event="([^"]+)"\} ([0-9]+)$/.exec(line);
    if (!match) continue;
    out[match[1]] = Number.parseInt(match[2], 10);
  }
  return out;
}

function getCounter(counters, name) {
  return counters[name] ?? 0;
}

async function httpGetText(url, { timeoutMs = 5_000 } = {}) {
  return await new Promise((resolve, reject) => {
    const req = http.get(url, (res) => {
      res.setEncoding("utf8");
      let body = "";
      res.on("data", (chunk) => {
        body += chunk;
      });
      res.on("end", () => {
        if (res.statusCode !== 200) {
          reject(new Error(`unexpected status ${res.statusCode ?? "unknown"} fetching ${url}`));
          return;
        }
        resolve(body);
      });
    });
    req.on("error", reject);
    req.setTimeout(timeoutMs, () => {
      req.destroy(new Error(`timed out fetching ${url}`));
    });
  });
}

async function getRelayEventCounters(port) {
  const metricsText = await httpGetText(`http://127.0.0.1:${port}/metrics`);
  return parseRelayEventCounters(metricsText);
}

async function waitForRelayEventCounterAtLeast(port, event, atLeast, { timeoutMs = 5_000 } = {}) {
  const started = Date.now();
  let counters = {};
  while (true) {
    counters = await getRelayEventCounters(port);
    if (getCounter(counters, event) >= atLeast) return counters;
    if (Date.now() - started > timeoutMs) return counters;
    await sleep(100);
  }
}

async function waitForRelayEventCounterEquals(port, event, want, { timeoutMs = 5_000 } = {}) {
  const started = Date.now();
  let counters = {};
  while (true) {
    counters = await getRelayEventCounters(port);
    if (getCounter(counters, event) === want) return counters;
    if (Date.now() - started > timeoutMs) return counters;
    await sleep(100);
  }
}

function waitForChildClose(child) {
  if (child.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const onDone = () => {
      cleanup();
      resolve();
    };
    const cleanup = () => {
      child.off("exit", onDone);
      child.off("close", onDone);
      child.off("error", onDone);
    };
    child.once("exit", onDone);
    child.once("close", onDone);
    child.once("error", onDone);

    // Handle races where the process exits between checking exitCode and
    // registering event listeners.
    if (child.exitCode !== null) onDone();
  });
}

async function stopChildProcess(child, { timeoutMs = 5_000 } = {}) {
  if (!child || child.exitCode !== null) return;

  try {
    child.kill("SIGTERM");
  } catch {
    // ignore
  }

  try {
    await Promise.race([waitForChildClose(child), sleep(timeoutMs).then(() => null)]);
  } catch {
    // ignore
  }

  if (child.exitCode !== null) return;

  try {
    child.kill("SIGKILL");
  } catch {
    // ignore
  }

  // Best-effort: don't hang teardown forever.
  await Promise.race([waitForChildClose(child), sleep(2_000)]);
}

async function startUdpEchoServer(socketType, host) {
  const socket = dgram.createSocket(socketType);
  socket.on("error", () => {
    // Ignore asynchronous socket errors. Test failures should be surfaced via
    // missing responses/timeouts, not by crashing the Node process.
  });
  socket.on("message", (msg, rinfo) => {
    socket.send(msg, rinfo.port, rinfo.address, () => {});
  });

  const bound = await new Promise((resolve) => {
    socket.once("error", () => resolve(false));
    socket.bind(0, host, () => resolve(true));
  });
  if (!bound) {
    await new Promise((resolve) => socket.close(resolve));
    return null;
  }
  const { port } = socket.address();
  return {
    port,
    close: () => new Promise((resolve) => socket.close(resolve)),
  };
}

async function startUdpEchoServerDifferentSourcePort(socketType, host) {
  const listener = dgram.createSocket(socketType);
  listener.on("error", () => {
    // ignore
  });
  const boundListener = await new Promise((resolve) => {
    listener.once("error", () => resolve(false));
    listener.bind(0, host, () => resolve(true));
  });
  if (!boundListener) {
    await new Promise((resolve) => listener.close(resolve));
    return null;
  }

  const responder = dgram.createSocket(socketType);
  responder.on("error", () => {
    // ignore
  });
  const boundResponder = await new Promise((resolve) => {
    responder.once("error", () => resolve(false));
    responder.bind(0, host, () => resolve(true));
  });
  if (!boundResponder) {
    await Promise.all([
      new Promise((resolve) => listener.close(resolve)),
      new Promise((resolve) => responder.close(resolve)),
    ]);
    return null;
  }

  listener.on("message", (msg, rinfo) => {
    responder.send(msg, rinfo.port, rinfo.address, () => {});
  });

  const { port } = listener.address();
  const { port: replyPort } = responder.address();
  return {
    port,
    replyPort,
    close: async () => {
      await Promise.all([
        new Promise((resolve) => listener.close(resolve)),
        new Promise((resolve) => responder.close(resolve)),
      ]);
    },
  };
}

async function startUdpServerWithDelayedRepeat(socketType, host, { delayMs, latePayload }) {
  const socket = dgram.createSocket(socketType);
  socket.on("error", () => {
    // ignore errors caused by races between timers and socket teardown
  });
  const bound = await new Promise((resolve) => {
    socket.once("error", () => resolve(false));
    socket.bind(0, host, () => resolve(true));
  });
  if (!bound) {
    await new Promise((resolve) => socket.close(resolve));
    return null;
  }

  let closed = false;
  let scheduled = false;
  let timer;
  socket.on("message", (msg, rinfo) => {
    try {
      socket.send(msg, rinfo.port, rinfo.address, () => {});
    } catch {
      // ignore
    }
    if (scheduled) return;
    scheduled = true;
    timer = setTimeout(() => {
      if (closed) return;
      try {
        socket.send(latePayload ?? msg, rinfo.port, rinfo.address);
      } catch {
        // ignore
      }
    }, delayMs);
    unrefBestEffort(timer);
  });

  const { port } = socket.address();
  return {
    port,
    close: () =>
      new Promise((resolve) => {
        closed = true;
        if (timer) clearTimeout(timer);
        socket.close(resolve);
      }),
  };
}

async function startWebServer() {
  const server = http.createServer((req, res) => {
    res.statusCode = 200;
    res.setHeader("content-type", "text/html; charset=utf-8");
    res.end("<!doctype html><title>webrtc-udp-relay e2e</title>");
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return {
    url: `http://127.0.0.1:${port}/`,
    close: () => new Promise((resolve, reject) => server.close((err) => (err ? reject(err) : resolve()))),
  };
}

const goReadyBinPromises = new Map();

async function getGoReadyBinaryPath({ name, pkg }) {
  const key = `${name}:${pkg}`;
  let promise = goReadyBinPromises.get(key);
  if (!promise) {
    promise = (async () => {
      const moduleDir = path.join(__dirname, "..", "..");
      const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "aero-webrtc-udp-relay-e2e-"));
      const binName = process.platform === "win32" ? `${name}.exe` : name;
      const binPath = path.join(tmpDir, binName);

      const build = spawnSync("go", ["build", "-o", binPath, pkg], {
        cwd: moduleDir,
        stdio: "inherit",
      });
      if (build.status !== 0) {
        await fs.rm(tmpDir, { recursive: true, force: true }).catch(() => {});
        throw new Error(`failed to build Go server ${pkg} (exit ${build.status ?? "unknown"})`);
      }

      process.once("exit", () => {
        try {
          fsSync.rmSync(tmpDir, { recursive: true, force: true });
        } catch {
          // ignore
        }
      });

      return binPath;
    })().catch((err) => {
      goReadyBinPromises.delete(key);
      throw err;
    });
    goReadyBinPromises.set(key, promise);
  }
  return promise;
}

async function spawnGoReadyServer({ name, pkg, env }) {
  const binPath = await getGoReadyBinaryPath({ name, pkg });

  const child = spawn(binPath, [], {
    env: {
      ...process.env,
      ...env,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stderr.on("data", (chunk) => {
    // Surface relay crashes in the test output.
    process.stderr.write(chunk);
  });

  const port = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`${name} did not start`)), 10_000);
    unrefBestEffort(timeout);
    let buffer = "";
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      while (true) {
        const newline = buffer.indexOf("\n");
        if (newline === -1) break;
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        const match = /^READY (\d+)$/.exec(line);
        if (!match) continue;
        clearTimeout(timeout);
        resolve(Number.parseInt(match[1], 10));
        return;
      }
    });

    child.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });

    child.on("close", (code) => {
      clearTimeout(timeout);
      reject(new Error(`${name} exited early (${code ?? "unknown"})`));
    });
  });

  return {
    port,
    kill: async () => {
      await stopChildProcess(child);
    },
  };
}

async function spawnL2BackendServer(extraEnv = {}) {
  return spawnGoReadyServer({
    name: "l2-backend-go",
    pkg: "./e2e/l2-backend-go",
    env: {
      BIND_HOST: "127.0.0.1",
      PORT: "0",
      // Prevent per-developer env leakage from changing backend auth behavior.
      REQUIRE_ORIGIN: "",
      REQUIRE_TOKEN: "",
      REQUIRE_COOKIE_NAME: "",
      REQUIRE_COOKIE_VALUE: "",
      ...extraEnv,
    },
  });
}

async function getFreePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close((err) => (err ? reject(err) : resolve(port)));
    });
  });
}

async function checkHealth(port) {
  return await new Promise((resolve) => {
    const req = http.get(`http://127.0.0.1:${port}/healthz`, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    req.once("error", () => resolve(false));
  });
}

async function waitForRelayReady(port, child, timeoutMs) {
  const started = Date.now();

  while (true) {
    if (child.exitCode !== null) {
      throw new Error(`relay exited early (${child.exitCode ?? "unknown"})`);
    }

    if (await checkHealth(port)) return;

    if (Date.now() - started > timeoutMs) {
      throw new Error("relay did not become ready");
    }

    await sleep(100);
  }
}

let relayBinPromise;
let relayBinTmpDir;

async function getRelayBinaryPath() {
  if (!relayBinPromise) {
    relayBinPromise = (async () => {
      const moduleDir = path.join(__dirname, "..", "..");
      const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "aero-webrtc-udp-relay-e2e-"));
      relayBinTmpDir = tmpDir;
      const binName = process.platform === "win32" ? "aero-webrtc-udp-relay.exe" : "aero-webrtc-udp-relay";
      const binPath = path.join(tmpDir, binName);

      const build = spawnSync("go", ["build", "-o", binPath, "./cmd/aero-webrtc-udp-relay"], {
        cwd: moduleDir,
        stdio: "inherit",
      });
      if (build.status !== 0) {
        await fs.rm(tmpDir, { recursive: true, force: true }).catch(() => {});
        throw new Error(`failed to build aero-webrtc-udp-relay (exit ${build.status ?? "unknown"})`);
      }

      return binPath;
    })().catch((err) => {
      // Allow retries if the initial build failed.
      relayBinPromise = null;
      throw err;
    });

    // Clean up the relay build dir at process exit. This is best-effort; use a
    // synchronous removal so it actually runs during the `exit` event.
    process.once("exit", () => {
      if (!relayBinTmpDir) return;
      try {
        fsSync.rmSync(relayBinTmpDir, { recursive: true, force: true });
      } catch {
        // ignore
      }
    });
  }
  return relayBinPromise;
}

async function spawnRelayServer(extraEnv = {}) {
  const moduleDir = path.join(__dirname, "..", "..");
  const relayBin = await getRelayBinaryPath();

  // There is a small race between allocating an ephemeral port and the child
  // process binding it; retry on failure to reduce test flakiness.
  let lastErr;
  for (let attempt = 0; attempt < 5; attempt++) {
    const port = await getFreePort();

    const child = spawn(relayBin, ["--listen-addr", `127.0.0.1:${port}`], {
      cwd: moduleDir,
      env: {
        ...process.env,
        // Keep tests deterministic even when the developer's shell environment
        // has L2 bridge env vars set.
        L2_BACKEND_WS_URL: "",
        L2_BACKEND_AUTH_FORWARD_MODE: "",
        L2_BACKEND_FORWARD_ORIGIN: "",
        L2_BACKEND_ORIGIN: "",
        L2_BACKEND_ORIGIN_OVERRIDE: "",
        L2_BACKEND_WS_ORIGIN: "",
        L2_BACKEND_TOKEN: "",
        L2_BACKEND_WS_TOKEN: "",
        L2_BACKEND_FORWARD_AERO_SESSION: "",
        L2_MAX_MESSAGE_BYTES: "",
        // Deterministic inbound allowlist / filtering settings.
        UDP_INBOUND_FILTER_MODE: "",
        UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: "",
        MAX_ALLOWED_REMOTES_PER_BINDING: "",
        UDP_BINDING_IDLE_TIMEOUT: "",
        UDP_READ_BUFFER_BYTES: "",
        DATACHANNEL_SEND_QUEUE_BYTES: "",
        MAX_DATAGRAM_PAYLOAD_BYTES: "",
        MAX_UDP_BINDINGS_PER_SESSION: "",
        // Deterministic quota/rate limiting settings.
        MAX_SESSIONS: "",
        SESSION_PREALLOC_TTL: "",
        MAX_UDP_PPS_PER_SESSION: "",
        MAX_UDP_BPS_PER_SESSION: "",
        MAX_UDP_PPS_PER_DEST: "",
        MAX_UNIQUE_DESTINATIONS_PER_SESSION: "",
        MAX_UDP_DEST_BUCKETS_PER_SESSION: "",
        MAX_DC_BPS_PER_SESSION: "",
        HARD_CLOSE_AFTER_VIOLATIONS: "",
        VIOLATION_WINDOW_SECONDS: "",
        // Deterministic signaling hardening settings.
        SIGNALING_AUTH_TIMEOUT: "",
        SIGNALING_WS_IDLE_TIMEOUT: "",
        SIGNALING_WS_PING_INTERVAL: "",
        MAX_SIGNALING_MESSAGE_BYTES: "",
        MAX_SIGNALING_MESSAGES_PER_SECOND: "",
        UDP_WS_IDLE_TIMEOUT: "",
        UDP_WS_PING_INTERVAL: "",
        AERO_WEBRTC_UDP_RELAY_ICE_GATHERING_TIMEOUT: "",
        AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT: "",
        AERO_WEBRTC_UDP_RELAY_LOG_FORMAT: "",
        AERO_WEBRTC_UDP_RELAY_MODE: "",
        // Prevent TURN REST config from leaking into localhost runs (these are
        // parsed/validated at startup even if unused by the tests).
        TURN_REST_SHARED_SECRET: "",
        TURN_REST_TTL_SECONDS: "",
        TURN_REST_USERNAME_PREFIX: "",
        TURN_REST_REALM: "",
        // Clear WebRTC ICE/network env vars that can break localhost connectivity
        // when a developer shell carries production NAT/listen settings.
        WEBRTC_NAT_1TO1_IPS: "",
        WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE: "",
        WEBRTC_UDP_LISTEN_IP: "",
        WEBRTC_UDP_PORT_MIN: "",
        WEBRTC_UDP_PORT_MAX: "",
        WEBRTC_SESSION_CONNECT_TIMEOUT: "",
        WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES: "",
        WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES: "",
        // Let the Playwright-served page (random localhost port) talk to the relay.
        ALLOWED_ORIGINS: "*",
        // Keep /webrtc/ice stable even when no STUN/TURN is configured.
        AERO_ICE_SERVERS_JSON: "[]",
        AERO_STUN_URLS: "",
        AERO_TURN_URLS: "",
        AERO_TURN_USERNAME: "",
        AERO_TURN_CREDENTIAL: "",
        // Avoid environment proxy leakage impacting backend WebSocket dialing.
        HTTP_PROXY: "",
        HTTPS_PROXY: "",
        ALL_PROXY: "",
        NO_PROXY: "",
        http_proxy: "",
        https_proxy: "",
        all_proxy: "",
        no_proxy: "",
        // Allow the UDP echo server on localhost.
        DESTINATION_POLICY_PRESET: "dev",
        ALLOW_PRIVATE_NETWORKS: "true",
        // Clear any per-deployment allow/deny lists so the dev preset is
        // deterministic in local/CI runs.
        UDP_DESTINATION_POLICY_PRESET: "",
        POLICY_PRESET: "",
        ALLOW_UDP_CIDRS: "",
        DENY_UDP_CIDRS: "",
        ALLOW_UDP_PORTS: "",
        DENY_UDP_PORTS: "",
        // Ensure IPv4 echo responses can be v2 once the client demonstrates v2 support.
        PREFER_V2: "true",
        // Auth is irrelevant for these tests, so disable it.
        AUTH_MODE: "none",
        API_KEY: "",
        JWT_SECRET: "",
        // Reduce noise in Playwright output.
        AERO_WEBRTC_UDP_RELAY_LOG_LEVEL: "error",
        ...extraEnv,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    // Surface relay crashes in the test output.
    // Note: the relay logs to stdout by default, so we must drain both streams to
    // avoid deadlocking on a full pipe buffer.
    child.stdout.on("data", (chunk) => process.stderr.write(chunk));
    child.stderr.on("data", (chunk) => process.stderr.write(chunk));

    try {
      await waitForRelayReady(port, child, 20_000);

      return {
        port,
        kill: async () => {
          await stopChildProcess(child);
        },
      };
    } catch (err) {
      lastErr = err;
      await stopChildProcess(child);
    }
  }

  throw lastErr ?? new Error("failed to start relay server");
}

test("relays a UDP datagram via a Chromium WebRTC DataChannel", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        const payload = new TextEncoder().encode("hello from chromium");
        const guestPort = 10_000;
        const frame = new Uint8Array(8 + payload.length);
        frame[0] = (guestPort >> 8) & 0xff;
        frame[1] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 2);
        frame[6] = (echoPort >> 8) & 0xff;
        frame[7] = echoPort & 0xff;
        frame.set(payload, 8);
        dc.send(frame);

        const echoedFrame = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        if (echoedFrame.length < 8) throw new Error("echoed frame too short");
        const echoedGuestPort = (echoedFrame[0] << 8) | echoedFrame[1];
        if (echoedGuestPort !== guestPort) throw new Error("guest port mismatch");
        const echoedIP = `${echoedFrame[2]}.${echoedFrame[3]}.${echoedFrame[4]}.${echoedFrame[5]}`;
        if (echoedIP !== "127.0.0.1") throw new Error("remote ip mismatch");
        const echoedRemotePort = (echoedFrame[6] << 8) | echoedFrame[7];
        if (echoedRemotePort !== echoPort) throw new Error("remote port mismatch");

        const echoedPayload = echoedFrame.slice(8);
        const echoedText = new TextDecoder().decode(echoedPayload);
        ws.close();
        pc.close();
        return echoedText;
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(echoed).toBe("hello from chromium");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("drops UDP replies from unexpected source ports over WebRTC by default (UDP_INBOUND_FILTER_MODE=address_and_port)", async ({ page }) => {
  const echo = await startUdpEchoServerDifferentSourcePort("udp4", "127.0.0.1");
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer();
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  expect(echo.replyPort).not.toBe(echo.port);

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        let ws;
        let pc;
        try {
          const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
          if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
            throw new Error("invalid ice server response");
          }
          const iceServers = iceResp.iceServers;

          ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
          await new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

          pc = new RTCPeerConnection({ iceServers });
          const pendingCandidates = [];
          let remoteDescriptionSet = false;
          const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
          dc.binaryType = "arraybuffer";

          const answerPromise = new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
            let answered = false;
            const onMessage = (event) => {
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error("invalid signaling message (not JSON)"));
                return;
              }

              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }

              if (msg?.type === "candidate") {
                if (!msg.candidate?.candidate) return;
                if (remoteDescriptionSet) {
                  pc.addIceCandidate(msg.candidate).catch(() => {});
                } else {
                  pendingCandidates.push(msg.candidate);
                }
                return;
              }

              if (msg?.type !== "answer") return;
              if (answered) return;
              answered = true;
              clearTimeout(timeout);
              resolve(msg);
            };
            ws.addEventListener("message", onMessage);
          });

          const offer = await pc.createOffer();
          await pc.setLocalDescription(offer);

          await new Promise((resolve) => {
            if (pc.iceGatheringState === "complete") return resolve();
            const onState = () => {
              if (pc.iceGatheringState !== "complete") return;
              pc.removeEventListener("icegatheringstatechange", onState);
              resolve();
            };
            pc.addEventListener("icegatheringstatechange", onState);
          });

          if (!pc.localDescription?.sdp) {
            throw new Error("missing local description");
          }

          ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

          const answerMsg = await answerPromise;
          if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
            throw new Error("invalid answer message shape");
          }

          await pc.setRemoteDescription(answerMsg.sdp);
          remoteDescriptionSet = true;
          for (const candidate of pendingCandidates) {
            await pc.addIceCandidate(candidate);
          }

          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
            dc.addEventListener(
              "open",
              () => {
                clearTimeout(timeout);
                resolve();
              },
              { once: true },
            );
            dc.addEventListener(
              "error",
              () => {
                clearTimeout(timeout);
                reject(new Error("datachannel error"));
              },
              { once: true },
            );
          });

          const payload = new TextEncoder().encode("hello from chromium unexpected source port");
          const guestPort = 10_000;
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);
          dc.send(frame);

          const echoedFrame = await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              if (done) return;
              done = true;
              cleanup();
              resolve(new Uint8Array(event.data));
            };
            const cleanup = () => {
              dc.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, 2_000);
            dc.addEventListener("message", onMessage);
          });

          return { received: echoedFrame !== null };
        } finally {
          ws?.close?.();
          pc?.close?.();
        }
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.received).toBe(false);

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const expectedMinDrops = beforeDrops + 1;
    const after = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, expectedMinDrops);
    expect(getCounter(after, allowlistDropMetric)).toBeGreaterThanOrEqual(expectedMinDrops);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("accepts UDP replies from unexpected source ports over WebRTC when UDP_INBOUND_FILTER_MODE=any", async ({ page }) => {
  const echo = await startUdpEchoServerDifferentSourcePort("udp4", "127.0.0.1");
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({ UDP_INBOUND_FILTER_MODE: "any" });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  expect(echo.replyPort).not.toBe(echo.port);

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        let ws;
        let pc;
        try {
          const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
          if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
            throw new Error("invalid ice server response");
          }
          const iceServers = iceResp.iceServers;

          ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
          await new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

          pc = new RTCPeerConnection({ iceServers });
          const pendingCandidates = [];
          let remoteDescriptionSet = false;
          const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
          dc.binaryType = "arraybuffer";

          const answerPromise = new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
            let answered = false;
            const onMessage = (event) => {
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error("invalid signaling message (not JSON)"));
                return;
              }

              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }

              if (msg?.type === "candidate") {
                if (!msg.candidate?.candidate) return;
                if (remoteDescriptionSet) {
                  pc.addIceCandidate(msg.candidate).catch(() => {});
                } else {
                  pendingCandidates.push(msg.candidate);
                }
                return;
              }

              if (msg?.type !== "answer") return;
              if (answered) return;
              answered = true;
              clearTimeout(timeout);
              resolve(msg);
            };
            ws.addEventListener("message", onMessage);
          });

          const offer = await pc.createOffer();
          await pc.setLocalDescription(offer);

          await new Promise((resolve) => {
            if (pc.iceGatheringState === "complete") return resolve();
            const onState = () => {
              if (pc.iceGatheringState !== "complete") return;
              pc.removeEventListener("icegatheringstatechange", onState);
              resolve();
            };
            pc.addEventListener("icegatheringstatechange", onState);
          });

          if (!pc.localDescription?.sdp) {
            throw new Error("missing local description");
          }

          ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

          const answerMsg = await answerPromise;
          if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
            throw new Error("invalid answer message shape");
          }

          await pc.setRemoteDescription(answerMsg.sdp);
          remoteDescriptionSet = true;
          for (const candidate of pendingCandidates) {
            await pc.addIceCandidate(candidate);
          }

          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
            dc.addEventListener(
              "open",
              () => {
                clearTimeout(timeout);
                resolve();
              },
              { once: true },
            );
            dc.addEventListener(
              "error",
              () => {
                clearTimeout(timeout);
                reject(new Error("datachannel error"));
              },
              { once: true },
            );
          });

          const payload = new TextEncoder().encode("hello from chromium any inbound filter");
          const guestPort = 10_000;
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);
          dc.send(frame);

          const echoedFrame = await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
            dc.addEventListener(
              "message",
              (event) => {
                clearTimeout(timeout);
                resolve(new Uint8Array(event.data));
              },
              { once: true },
            );
          });

          if (echoedFrame.length < 8) throw new Error("echoed frame too short");
          const echoedGuestPort = (echoedFrame[0] << 8) | echoedFrame[1];
          if (echoedGuestPort !== guestPort) throw new Error("guest port mismatch");
          const echoedIP = `${echoedFrame[2]}.${echoedFrame[3]}.${echoedFrame[4]}.${echoedFrame[5]}`;
          if (echoedIP !== "127.0.0.1") throw new Error("remote ip mismatch");

          const echoedRemotePort = (echoedFrame[6] << 8) | echoedFrame[7];
          const echoedText = new TextDecoder().decode(echoedFrame.slice(8));
          return { echoedRemotePort, echoedText };
        } finally {
          ws?.close?.();
          pc?.close?.();
        }
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.echoedText).toBe("hello from chromium any inbound filter");
    expect(res.echoedRemotePort).toBe(echo.replyPort);

    const counters = await waitForRelayEventCounterEquals(relay.port, allowlistDropMetric, 0);
    expect(getCounter(counters, allowlistDropMetric)).toBe(0);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("expires UDP remote allowlist entries over WebRTC after UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT", async ({ page }) => {
  const ttlMs = 250;
  const lateSendDelayMs = 900;
  const echo = await startUdpServerWithDelayedRepeat("udp4", "127.0.0.1", {
    delayMs: lateSendDelayMs,
    latePayload: Buffer.from("late datagram"),
  });
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({ UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: `${ttlMs}ms` });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        let pc;
        try {
          const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
          const iceServers = Array.isArray(iceResp?.iceServers) ? iceResp.iceServers : [];

          pc = new RTCPeerConnection({ iceServers });
          const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
          dc.binaryType = "arraybuffer";

          const offer = await pc.createOffer();
          await pc.setLocalDescription(offer);

          await new Promise((resolve) => {
            if (pc.iceGatheringState === "complete") return resolve();
            const onState = () => {
              if (pc.iceGatheringState !== "complete") return;
              pc.removeEventListener("icegatheringstatechange", onState);
              resolve();
            };
            pc.addEventListener("icegatheringstatechange", onState);
          });

          if (!pc.localDescription?.sdp) {
            throw new Error("missing local description");
          }

          const offerResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/offer`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ type: "offer", sdp: pc.localDescription.sdp }),
          });
          if (!offerResp.ok) {
            throw new Error(`unexpected /webrtc/offer status ${offerResp.status}`);
          }
          const offerJSON = await offerResp.json();
          if (!offerJSON?.sdp?.sdp) {
            throw new Error("invalid /webrtc/offer response");
          }
          await pc.setRemoteDescription(offerJSON.sdp);

          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
            dc.addEventListener(
              "open",
              () => {
                clearTimeout(timeout);
                resolve();
              },
              { once: true },
            );
            dc.addEventListener(
              "error",
              () => {
                clearTimeout(timeout);
                reject(new Error("datachannel error"));
              },
              { once: true },
            );
          });

          const waitForDatagram = async ({ timeoutMs }) =>
            await new Promise((resolve) => {
              let timeout;
              let done = false;
              const onMessage = (event) => {
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(event.data));
              };
              const cleanup = () => {
                dc.removeEventListener("message", onMessage);
                clearTimeout(timeout);
              };
              timeout = setTimeout(() => {
                if (done) return;
                done = true;
                cleanup();
                resolve(null);
              }, timeoutMs);
              dc.addEventListener("message", onMessage);
            });

          const guestPort = 10_000;
          const payload = new TextEncoder().encode("first datagram");
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);

          const firstPromise = waitForDatagram({ timeoutMs: 10_000 });
          dc.send(frame);
          const first = await firstPromise;
          if (!first) throw new Error("timed out waiting for first echoed datagram");
          if (first.length < 8) throw new Error("echoed frame too short");
          const firstText = new TextDecoder().decode(first.slice(8));

          // The delayed datagram is emitted ~900ms after the first send; keep a
          // conservative buffer but avoid waiting multiple seconds in the common
          // (expected) drop case.
          const second = await waitForDatagram({ timeoutMs: 1_500 });
          const secondText = second ? new TextDecoder().decode(second.slice(8)) : null;
          return { firstText, gotSecond: second !== null, secondText };
        } finally {
          pc?.close?.();
        }
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.firstText).toBe("first datagram");
    if (res.gotSecond) {
      throw new Error(`expected allowlist entry to expire; received second datagram: ${res.secondText ?? "<unknown>"}`);
    }

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const after = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, beforeDrops + 1, { timeoutMs: 8_000 });
    expect(getCounter(after, allowlistDropMetric)).toBeGreaterThanOrEqual(beforeDrops + 1);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("evicts UDP remote allowlist entries over WebRTC when MAX_ALLOWED_REMOTES_PER_BINDING is exceeded", async ({ page }) => {
  const echoA = await startUdpServerWithDelayedRepeat("udp4", "127.0.0.1", {
    delayMs: 2_000,
    latePayload: Buffer.from("late from A"),
  });
  const echoB = await startUdpEchoServer("udp4", "127.0.0.1");
  test.skip(!echoA || !echoB, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({
    MAX_ALLOWED_REMOTES_PER_BINDING: "1",
    UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: "10s",
  });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  const allowlistEvictMetric = "udp_remote_allowlist_evictions_total";

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPortA, echoPortB }) => {
        let pc;
        try {
          const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
          const iceServers = Array.isArray(iceResp?.iceServers) ? iceResp.iceServers : [];

          pc = new RTCPeerConnection({ iceServers });
          const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
          dc.binaryType = "arraybuffer";

          const offer = await pc.createOffer();
          await pc.setLocalDescription(offer);

          await new Promise((resolve) => {
            if (pc.iceGatheringState === "complete") return resolve();
            const onState = () => {
              if (pc.iceGatheringState !== "complete") return;
              pc.removeEventListener("icegatheringstatechange", onState);
              resolve();
            };
            pc.addEventListener("icegatheringstatechange", onState);
          });

          if (!pc.localDescription?.sdp) {
            throw new Error("missing local description");
          }

          const offerResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/offer`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ type: "offer", sdp: pc.localDescription.sdp }),
          });
          if (!offerResp.ok) {
            throw new Error(`unexpected /webrtc/offer status ${offerResp.status}`);
          }
          const offerJSON = await offerResp.json();
          if (!offerJSON?.sdp?.sdp) {
            throw new Error("invalid /webrtc/offer response");
          }
          await pc.setRemoteDescription(offerJSON.sdp);

          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
            dc.addEventListener(
              "open",
              () => {
                clearTimeout(timeout);
                resolve();
              },
              { once: true },
            );
            dc.addEventListener(
              "error",
              () => {
                clearTimeout(timeout);
                reject(new Error("datachannel error"));
              },
              { once: true },
            );
          });

          const waitForDatagram = async ({ timeoutMs }) =>
            await new Promise((resolve) => {
              let timeout;
              let done = false;
              const onMessage = (event) => {
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(event.data));
              };
              const cleanup = () => {
                dc.removeEventListener("message", onMessage);
                clearTimeout(timeout);
              };
              timeout = setTimeout(() => {
                if (done) return;
                done = true;
                cleanup();
                resolve(null);
              }, timeoutMs);
              dc.addEventListener("message", onMessage);
            });

          const guestPort = 10_000;
          const buildFrame = (remotePort, text) => {
            const payload = new TextEncoder().encode(text);
            const frame = new Uint8Array(8 + payload.length);
            frame[0] = (guestPort >> 8) & 0xff;
            frame[1] = guestPort & 0xff;
            frame.set([127, 0, 0, 1], 2);
            frame[6] = (remotePort >> 8) & 0xff;
            frame[7] = remotePort & 0xff;
            frame.set(payload, 8);
            return frame;
          };

          const sendAndRecvText = async (remotePort, text) => {
            const frame = buildFrame(remotePort, text);
            const respPromise = waitForDatagram({ timeoutMs: 10_000 });
            dc.send(frame);
            const resp = await respPromise;
            if (!resp) throw new Error(`timed out waiting for echoed datagram for ${text}`);
            if (resp.length < 8) throw new Error("echoed frame too short");
            return new TextDecoder().decode(resp.slice(8));
          };

          const textA = await sendAndRecvText(echoPortA, "hello A");
          const textB = await sendAndRecvText(echoPortB, "hello B");

          // If eviction worked, this should be dropped and we should not receive it.
          // The delayed datagram is emitted ~2s after the first send; keep enough
          // headroom for scheduling jitter while avoiding a long timeout on the
          // expected drop path.
          const late = await waitForDatagram({ timeoutMs: 2_500 });
          const lateText = late ? new TextDecoder().decode(late.slice(8)) : null;
          return { textA, textB, gotLate: late !== null, lateText };
        } finally {
          pc?.close?.();
        }
      },
      { relayPort: relay.port, echoPortA: echoA.port, echoPortB: echoB.port },
    );

    expect(res.textA).toBe("hello A");
    expect(res.textB).toBe("hello B");
    if (res.gotLate) {
      throw new Error(`expected allowlist entry A to be evicted; received datagram: ${res.lateText ?? "<unknown>"}`);
    }

    const beforeEvictions = getCounter(before, allowlistEvictMetric);
    const afterEvictions = await waitForRelayEventCounterAtLeast(relay.port, allowlistEvictMetric, beforeEvictions + 1);
    expect(getCounter(afterEvictions, allowlistEvictMetric)).toBeGreaterThanOrEqual(beforeEvictions + 1);

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const afterDrops = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, beforeDrops + 1, { timeoutMs: 8_000 });
    expect(getCounter(afterDrops, allowlistDropMetric)).toBeGreaterThanOrEqual(beforeDrops + 1);
  } finally {
    await Promise.all([web.close(), relay.kill(), echoA?.close(), echoB?.close()]);
  }
});

test("relays a UDP datagram via a Chromium WebRTC DataChannel using negotiated v2 IPv4 framing", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        const payload = new TextEncoder().encode("hello from chromium v2 ipv4");
        const guestPort = 10_000;

        // v2 frame: magic (0xA2) + version (0x02) + af (0x04) + type (0x00)
        // + guest_port (u16) + remote_ip (4B) + remote_port (u16) + payload.
        const frame = new Uint8Array(12 + payload.length);
        frame[0] = 0xa2;
        frame[1] = 0x02;
        frame[2] = 0x04;
        frame[3] = 0x00;
        frame[4] = (guestPort >> 8) & 0xff;
        frame[5] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 6);
        frame[10] = (echoPort >> 8) & 0xff;
        frame[11] = echoPort & 0xff;
        frame.set(payload, 12);
        dc.send(frame);

        const echoedFrame = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        if (echoedFrame.length < 12) throw new Error("echoed v2 frame too short");
        if (echoedFrame[0] !== 0xa2 || echoedFrame[1] !== 0x02 || echoedFrame[2] !== 0x04 || echoedFrame[3] !== 0x00) {
          throw new Error("echoed v2 header mismatch");
        }

        const echoedGuestPort = (echoedFrame[4] << 8) | echoedFrame[5];
        if (echoedGuestPort !== guestPort) throw new Error("guest port mismatch");
        const echoedIP = `${echoedFrame[6]}.${echoedFrame[7]}.${echoedFrame[8]}.${echoedFrame[9]}`;
        if (echoedIP !== "127.0.0.1") throw new Error("remote ip mismatch");
        const echoedRemotePort = (echoedFrame[10] << 8) | echoedFrame[11];
        if (echoedRemotePort !== echoPort) throw new Error("remote port mismatch");

        const echoedPayload = echoedFrame.slice(12);
        const echoedText = new TextDecoder().decode(echoedPayload);
        ws.close();
        pc.close();
        return echoedText;
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(echoed).toBe("hello from chromium v2 ipv4");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("authenticates WebRTC /webrtc/ice + /webrtc/signal with AUTH_MODE=jwt", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const token = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const invalidToken = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: `${jwtSecret}-wrong`,
  });

  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort, token, invalidToken }) => {
        const assertNoStoreHeaders = (resp) => {
          const cacheControl = resp.headers.get("cache-control");
          if (cacheControl !== "no-store") {
            throw new Error(`expected Cache-Control=no-store, got ${cacheControl ?? "<missing>"}`);
          }
          const pragma = resp.headers.get("pragma");
          if (pragma !== "no-cache") {
            throw new Error(`expected Pragma=no-cache, got ${pragma ?? "<missing>"}`);
          }
          const expires = resp.headers.get("expires");
          if (expires !== "0") {
            throw new Error(`expected Expires=0, got ${expires ?? "<missing>"}`);
          }
        };

        const unauth = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`);
        assertNoStoreHeaders(unauth);
        const unauthStatus = unauth.status;
        const unauthBody = await unauth.json().catch(() => null);

        const invalidAuth = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: {
            Authorization: `Bearer ${invalidToken}`,
          },
        });
        assertNoStoreHeaders(invalidAuth);
        const invalidAuthStatus = invalidAuth.status;
        const invalidAuthBody = await invalidAuth.json().catch(() => null);

        const apiKeyAuth = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: {
            "X-API-Key": token,
          },
        });
        assertNoStoreHeaders(apiKeyAuth);
        const apiKeyAuthStatus = apiKeyAuth.status;

        const authHeaderAPIKeyAuthStatus = (
          await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
            headers: {
              Authorization: `ApiKey ${token}`,
            },
          })
        );
        assertNoStoreHeaders(authHeaderAPIKeyAuthStatus);
        const authHeaderAPIKeyAuthStatusCode = authHeaderAPIKeyAuthStatus.status;

        const queryTokenAuthStatus = (
          await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice?token=${encodeURIComponent(token)}`)
        );
        assertNoStoreHeaders(queryTokenAuthStatus);
        const queryTokenAuthStatusCode = queryTokenAuthStatus.status;
        const queryAPIKeyAuthStatus = (
          await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice?apiKey=${encodeURIComponent(token)}`)
        );
        assertNoStoreHeaders(queryAPIKeyAuthStatus);
        const queryAPIKeyAuthStatusCode = queryAPIKeyAuthStatus.status;

        const authIceRes = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: {
            Authorization: `Bearer ${token}`,
          },
        });
        assertNoStoreHeaders(authIceRes);
        const authStatus = authIceRes.status;
        const iceResp = await authIceRes.json();
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        // WebSocket upgrade requests cannot include arbitrary headers, so we
        // authenticate using the first control-plane message.
        ws.send(JSON.stringify({ type: "auth", token }));

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        const payload = new TextEncoder().encode("hello from chromium jwt");
        const guestPort = 10_000;
        const frame = new Uint8Array(8 + payload.length);
        frame[0] = (guestPort >> 8) & 0xff;
        frame[1] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 2);
        frame[6] = (echoPort >> 8) & 0xff;
        frame[7] = echoPort & 0xff;
        frame.set(payload, 8);
        dc.send(frame);

        const echoedFrame = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        if (echoedFrame.length < 8) throw new Error("echoed frame too short");
        const echoedPayload = echoedFrame.slice(8);
        const echoedText = new TextDecoder().decode(echoedPayload);

        ws.close();
        pc.close();
        return {
          unauthStatus,
          unauthBody,
          invalidAuthStatus,
          invalidAuthBody,
          apiKeyAuthStatus,
          authHeaderAPIKeyAuthStatus: authHeaderAPIKeyAuthStatusCode,
          queryTokenAuthStatus: queryTokenAuthStatusCode,
          queryAPIKeyAuthStatus: queryAPIKeyAuthStatusCode,
          authStatus,
          echoedText,
        };
      },
      { relayPort: relay.port, echoPort: echo.port, token, invalidToken },
    );

    expect(res.unauthStatus).toBe(401);
    expect(res.unauthBody?.code).toBe("unauthorized");
    expect(res.invalidAuthStatus).toBe(401);
    expect(res.invalidAuthBody?.code).toBe("unauthorized");
    expect(res.apiKeyAuthStatus).toBe(200);
    expect(res.authHeaderAPIKeyAuthStatus).toBe(200);
    expect(res.queryTokenAuthStatus).toBe(200);
    expect(res.queryAPIKeyAuthStatus).toBe(200);
    expect(res.authStatus).toBe(200);
    expect(res.echoedText).toBe("hello from chromium jwt");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("rejects concurrent WebRTC sessions with the same JWT sid", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const tokenA = mintHS256JWT({
    sid: "sess_e2e",
    iat: now - 10,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const tokenB = mintHS256JWT({
    sid: "sess_e2e",
    iat: now - 9,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });

  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, tokenA, tokenB }) => {
        const waitForOpen = (ws) =>
          new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

        const waitForClosed = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for websocket close")), 10_000);
            ws.addEventListener(
              "close",
              (event) => {
                clearTimeout(timeout);
                resolve({ closeCode: event.code, closeReason: event.reason });
              },
              { once: true },
            );
          });

        const waitForAnswer = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
            const onMessage = (event) => {
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                return;
              }
              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }
              if (msg?.type !== "answer") return;
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              resolve(msg);
            };
            ws.addEventListener("message", onMessage);
          });

        const waitForClose = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for error + close")), 10_000);
            let errMsg;
            let closed;
            const maybeDone = () => {
              if (!closed) return;
              cleanup();
              resolve({ errMsg, closeCode: closed.code, closeReason: closed.reason });
            };
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                return;
              }
              if (msg?.type === "error") {
                errMsg = msg;
              }
            };
            const onClose = (event) => {
              closed = event;
              maybeDone();
            };
            const cleanup = () => {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              ws.removeEventListener("close", onClose);
            };
            ws.addEventListener("message", onMessage);
            ws.addEventListener("close", onClose);
          });

        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: { Authorization: `Bearer ${tokenA}` },
        }).then((r) => r.json());
        const iceServers = iceResp.iceServers ?? [];

        const ws1 = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await waitForOpen(ws1);
        ws1.send(JSON.stringify({ type: "auth", token: tokenA }));

        const pc = new RTCPeerConnection({ iceServers });
        pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        const offerSDP = pc.localDescription.sdp;
        ws1.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: offerSDP } }));
        await waitForAnswer(ws1);

        const ws2 = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        const ws2ClosePromise = waitForClose(ws2);
        await waitForOpen(ws2);
        ws2.send(JSON.stringify({ type: "auth", token: tokenB }));
        // A valid SDP isn't required because the relay rejects on quota allocation
        // before setting the remote description. Keep this deterministic and fast.
        ws2.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: "v=0" } }));
        const ws2Res = await ws2ClosePromise;

        const ws1ClosedPromise = waitForClosed(ws1);
        ws1.close();
        pc.close();
        await ws1ClosedPromise;

        // The JWT sid quota key should be released once the first session is closed.
        const ws3 = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await waitForOpen(ws3);
        ws3.send(JSON.stringify({ type: "auth", token: tokenB }));
        ws3.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: offerSDP } }));
        await waitForAnswer(ws3);
        const ws3ClosedPromise = waitForClosed(ws3);
        ws3.close();
        await ws3ClosedPromise;

        return { ws2Res, reused: true };
      },
      { relayPort: relay.port, tokenA, tokenB },
    );

    expect(res.ws2Res.errMsg?.type).toBe("error");
    expect(res.ws2Res.errMsg?.code).toBe("session_already_active");
    expect(res.ws2Res.closeCode).toBe(1008);
    expect(["session_already_active", "session already active", ""]).toContain(res.ws2Res.closeReason ?? "");
    expect(res.reused).toBe(true);
  } finally {
    await Promise.all([web.close(), relay.kill()]);
  }
});

test("rejects concurrent HTTP session allocations with the same JWT sid", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const tokenA = mintHS256JWT({
    sid: "sess_e2e_http",
    iat: now - 10,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const tokenB = mintHS256JWT({
    sid: "sess_e2e_http",
    iat: now - 9,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });

  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, tokenA, tokenB }) => {
        const postJSON = async (path, body) => {
          const resp = await fetch(`http://127.0.0.1:${relayPort}${path}`, {
            method: "POST",
            headers: {
              Authorization: `Bearer ${tokenB}`,
              "Content-Type": "application/json",
            },
            body: JSON.stringify(body),
          });
          return { status: resp.status, json: await resp.json().catch(() => null) };
        };

        const sess1 = await fetch(`http://127.0.0.1:${relayPort}/session`, {
          method: "POST",
          headers: {
            Authorization: `Bearer ${tokenA}`,
          },
        });
        const sess1Status = sess1.status;
        const sess1Body = await sess1.text();

        const sess2 = await fetch(`http://127.0.0.1:${relayPort}/session`, {
          method: "POST",
          headers: {
            Authorization: `Bearer ${tokenB}`,
          },
        });
        const sess2Status = sess2.status;
        const sess2JSON = await sess2.json().catch(() => null);

        const webrtcOffer = await postJSON("/webrtc/offer", { type: "offer", sdp: "v=0" });
        const offerV1 = await postJSON("/offer", { version: 1, offer: { type: "offer", sdp: "v=0" } });

        return { sess1Status, sess1Body, sess2Status, sess2JSON, webrtcOffer, offerV1 };
      },
      { relayPort: relay.port, tokenA, tokenB },
    );

    expect(res.sess1Status).toBe(201);
    expect(res.sess1Body).toMatch(/^[0-9a-f]{32}$/);

    expect(res.sess2Status).toBe(409);
    expect(res.sess2JSON?.code).toBe("session_already_active");

    expect(res.webrtcOffer.status).toBe(409);
    expect(res.webrtcOffer.json?.code).toBe("session_already_active");

    expect(res.offerV1.status).toBe(409);
    expect(res.offerV1.json?.code).toBe("session_already_active");
  } finally {
    await Promise.all([web.close(), relay.kill()]);
  }
});

test("releases JWT sid after /session preallocation TTL expires", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const tokenA = mintHS256JWT({
    sid: "sess_e2e_prealloc_ttl",
    iat: now - 10,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const tokenB = mintHS256JWT({
    sid: "sess_e2e_prealloc_ttl",
    iat: now - 9,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });

  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
    SESSION_PREALLOC_TTL: "1s",
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, tokenA, tokenB }) => {
        const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
        const postSession = async (token) => {
          const resp = await fetch(`http://127.0.0.1:${relayPort}/session`, {
            method: "POST",
            headers: {
              Authorization: `Bearer ${token}`,
            },
          });
          const text = await resp.text();
          let json = null;
          try {
            json = JSON.parse(text);
          } catch {
            // ignore
          }
          return { status: resp.status, text, json };
        };

        const first = await postSession(tokenA);
        const second = await postSession(tokenB);

        // Wait until the preallocated session expires and the stable sid key is released.
        let third = null;
        const deadline = Date.now() + 5_000;
        while (Date.now() < deadline) {
          await sleep(100);
          third = await postSession(tokenB);
          if (third.status === 201) break;
        }

        return { first, second, third };
      },
      { relayPort: relay.port, tokenA, tokenB },
    );

    expect(res.first.status).toBe(201);
    expect(res.first.text.trim()).toMatch(/^[0-9a-f]{32}$/);

    expect(res.second.status).toBe(409);
    expect(res.second.json?.code).toBe("session_already_active");

    expect(res.third?.status).toBe(201);
    expect(res.third?.text.trim()).toMatch(/^[0-9a-f]{32}$/);
  } finally {
    await Promise.all([web.close(), relay.kill()]);
  }
});

test("rejects unauthorized /webrtc/signal WebSocket messages with AUTH_MODE=jwt", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const token = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const invalidToken = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: `${jwtSecret}-wrong`,
  });

  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, token, invalidToken }) => {
        const waitForOpen = (ws) =>
          new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

        const waitForClose = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for error + close")), 10_000);
            let errMsg;
            let closed;
            const maybeDone = () => {
              if (!closed) return;
              cleanup();
              resolve({ errMsg, closeCode: closed.code, closeReason: closed.reason });
            };
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                return;
              }
              if (msg?.type === "error") {
                errMsg = msg;
              }
            };
            const onClose = (event) => {
              closed = event;
              maybeDone();
            };
            const cleanup = () => {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              ws.removeEventListener("close", onClose);
            };
            ws.addEventListener("message", onMessage);
            ws.addEventListener("close", onClose);
          });

        const unauthWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        const unauthPromise = waitForClose(unauthWS);
        await waitForOpen(unauthWS);
        unauthWS.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: "v=0" } }));
        const unauth = await unauthPromise;

        const invalidWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        const invalidPromise = waitForClose(invalidWS);
        await waitForOpen(invalidWS);
        invalidWS.send(JSON.stringify({ type: "auth", token: invalidToken }));
        const invalid = await invalidPromise;

        const authAPIKeyWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        const authAPIKeyPromise = waitForClose(authAPIKeyWS);
        await waitForOpen(authAPIKeyWS);
        authAPIKeyWS.send(JSON.stringify({ type: "auth", apiKey: token }));
        // Candidate-before-offer is a deterministic "unexpected_message" rejection
        // that only happens after authentication succeeds.
        authAPIKeyWS.send(
          JSON.stringify({
            type: "candidate",
            candidate: { candidate: "candidate:0 1 UDP 2122252543 127.0.0.1 12345 typ host" },
          }),
        );
        const authAPIKey = await authAPIKeyPromise;

        const invalidQueryWS = new WebSocket(
          `ws://127.0.0.1:${relayPort}/webrtc/signal?token=${encodeURIComponent(invalidToken)}`,
        );
        const invalidQueryPromise = waitForClose(invalidQueryWS);
        await waitForOpen(invalidQueryWS);
        const invalidQuery = await invalidQueryPromise;

        const queryWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal?token=${encodeURIComponent(token)}`);
        const queryPromise = waitForClose(queryWS);
        await waitForOpen(queryWS);
        // No auth message needed when query-string credentials are provided.
        queryWS.send(
          JSON.stringify({
            type: "candidate",
            candidate: { candidate: "candidate:0 1 UDP 2122252543 127.0.0.1 12345 typ host" },
          }),
        );
        const query = await queryPromise;

        const invalidAPIKeyQueryWS = new WebSocket(
          `ws://127.0.0.1:${relayPort}/webrtc/signal?apiKey=${encodeURIComponent(invalidToken)}`,
        );
        const invalidAPIKeyQueryPromise = waitForClose(invalidAPIKeyQueryWS);
        await waitForOpen(invalidAPIKeyQueryWS);
        const invalidAPIKeyQuery = await invalidAPIKeyQueryPromise;

        const apiKeyQueryWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal?apiKey=${encodeURIComponent(token)}`);
        const apiKeyQueryPromise = waitForClose(apiKeyQueryWS);
        await waitForOpen(apiKeyQueryWS);
        apiKeyQueryWS.send(
          JSON.stringify({
            type: "candidate",
            candidate: { candidate: "candidate:0 1 UDP 2122252543 127.0.0.1 12345 typ host" },
          }),
        );
        const apiKeyQuery = await apiKeyQueryPromise;

        const mismatchWS = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        const mismatchPromise = waitForClose(mismatchWS);
        await waitForOpen(mismatchWS);
        // If both token and apiKey are provided, they must match.
        mismatchWS.send(JSON.stringify({ type: "auth", token, apiKey: `${token}-mismatch` }));
        const mismatch = await mismatchPromise;

        return { unauth, invalid, authAPIKey, invalidQuery, query, invalidAPIKeyQuery, apiKeyQuery, mismatch };
      },
      { relayPort: relay.port, token, invalidToken },
    );

    if (res.unauth.errMsg) {
      expect(res.unauth.errMsg.code).toBe("unauthorized");
    } else {
      // Per PROTOCOL.md, the server may close without an error message.
      expect(["authentication required", "unauthorized", ""]).toContain(res.unauth.closeReason ?? "");
    }
    expect(res.unauth.closeCode).toBe(1008);

    if (res.invalid.errMsg) {
      expect(res.invalid.errMsg.code).toBe("unauthorized");
    } else {
      expect(["unauthorized", ""]).toContain(res.invalid.closeReason ?? "");
    }
    expect(res.invalid.closeCode).toBe(1008);

    if (res.authAPIKey.errMsg) {
      expect(res.authAPIKey.errMsg.code).toBe("unexpected_message");
    } else {
      expect(["unexpected_message", "unexpected message", ""]).toContain(res.authAPIKey.closeReason ?? "");
    }
    expect(res.authAPIKey.closeCode).toBe(1008);

    if (res.invalidQuery.errMsg) {
      expect(res.invalidQuery.errMsg.code).toBe("unauthorized");
    } else {
      expect(["unauthorized", ""]).toContain(res.invalidQuery.closeReason ?? "");
    }
    expect(res.invalidQuery.closeCode).toBe(1008);

    if (res.query.errMsg) {
      expect(res.query.errMsg.code).toBe("unexpected_message");
    } else {
      expect(["unexpected_message", "unexpected message", ""]).toContain(res.query.closeReason ?? "");
    }
    expect(res.query.closeCode).toBe(1008);

    if (res.invalidAPIKeyQuery.errMsg) {
      expect(res.invalidAPIKeyQuery.errMsg.code).toBe("unauthorized");
    } else {
      expect(["unauthorized", ""]).toContain(res.invalidAPIKeyQuery.closeReason ?? "");
    }
    expect(res.invalidAPIKeyQuery.closeCode).toBe(1008);

    if (res.apiKeyQuery.errMsg) {
      expect(res.apiKeyQuery.errMsg.code).toBe("unexpected_message");
    } else {
      expect(["unexpected_message", "unexpected message", ""]).toContain(res.apiKeyQuery.closeReason ?? "");
    }
    expect(res.apiKeyQuery.closeCode).toBe(1008);

    if (res.mismatch.errMsg) {
      expect(res.mismatch.errMsg.code).toBe("bad_message");
    } else {
      expect(["bad_message", "bad message", ""]).toContain(res.mismatch.closeReason ?? "");
    }
    expect(res.mismatch.closeCode).toBe(1008);
  } finally {
    await Promise.all([web.close(), relay.kill()]);
  }
});

test("relays a UDP datagram to an IPv6 destination via v2 framing", async ({ page }) => {
  const echo = await startUdpEchoServer("udp6", "::1");
  test.skip(!echo, "ipv6 not supported in test environment");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        const dc = pc.createDataChannel("udp", { ordered: false, maxRetransmits: 0 });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        const payload = new TextEncoder().encode("hello from chromium ipv6");
        const guestPort = 10_000;

        // v2 frame: magic (0xA2) + version (0x02) + af (0x06) + reserved (0)
        // + guest_port (u16) + remote_ip (16B) + remote_port (u16) + payload.
        const frame = new Uint8Array(UDP_RELAY_V2_IPV6_HEADER_BYTES + payload.length);
        frame[0] = 0xa2;
        frame[1] = 0x02;
        frame[2] = 0x06;
        frame[3] = 0x00;
        frame[UDP_RELAY_V2_GUEST_PORT_OFFSET] = (guestPort >> 8) & 0xff;
        frame[UDP_RELAY_V2_GUEST_PORT_OFFSET + 1] = guestPort & 0xff;
        // ::1
        frame[UDP_RELAY_V2_REMOTE_IP_OFFSET + UDP_RELAY_V2_IPV6_ADDR_BYTES - 1] = 1;
        frame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6] = (echoPort >> 8) & 0xff;
        frame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6 + 1] = echoPort & 0xff;
        frame.set(payload, UDP_RELAY_V2_IPV6_HEADER_BYTES);
        dc.send(frame);

        const echoedFrame = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        if (echoedFrame.length < UDP_RELAY_V2_IPV6_HEADER_BYTES) throw new Error("echoed frame too short");
        if (echoedFrame[0] !== 0xa2 || echoedFrame[1] !== 0x02 || echoedFrame[2] !== 0x06 || echoedFrame[3] !== 0x00) {
          throw new Error("v2 header mismatch");
        }

        const echoedGuestPort =
          (echoedFrame[UDP_RELAY_V2_GUEST_PORT_OFFSET] << 8) | echoedFrame[UDP_RELAY_V2_GUEST_PORT_OFFSET + 1];
        if (echoedGuestPort !== guestPort) throw new Error("guest port mismatch");
        for (let i = UDP_RELAY_V2_REMOTE_IP_OFFSET; i < UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6 - 1; i++) {
          if (echoedFrame[i] !== 0) throw new Error("remote ip mismatch");
        }
        if (echoedFrame[UDP_RELAY_V2_REMOTE_IP_OFFSET + UDP_RELAY_V2_IPV6_ADDR_BYTES - 1] !== 1)
          throw new Error("remote ip mismatch");

        const echoedRemotePort =
          (echoedFrame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6] << 8) |
          echoedFrame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6 + 1];
        if (echoedRemotePort !== echoPort) throw new Error("remote port mismatch");

        const echoedPayload = echoedFrame.slice(UDP_RELAY_V2_IPV6_HEADER_BYTES);
        const echoedText = new TextDecoder().decode(echoedPayload);
        ws.close();
        pc.close();
        return echoedText;
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(echoed).toBe("hello from chromium ipv6");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("relays UDP datagrams via the /udp WebSocket fallback (v1 + v2)", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const sendAndRecv = async (frame) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    throw new Error(`unexpected websocket text message: ${data}`);
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  throw new Error(`unexpected websocket text message: ${data}`);
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              reject(new Error("timed out waiting for echoed datagram"));
            }, 10_000);
            ws.addEventListener("message", onMessage);
            ws.send(frame);
          });

        const guestPort = 10_000;

        const payload1 = new TextEncoder().encode("hello from websocket v1");
        const frame1 = new Uint8Array(8 + payload1.length);
        frame1[0] = (guestPort >> 8) & 0xff;
        frame1[1] = guestPort & 0xff;
        frame1.set([127, 0, 0, 1], 2);
        frame1[6] = (echoPort >> 8) & 0xff;
        frame1[7] = echoPort & 0xff;
        frame1.set(payload1, 8);

        const echoedFrame1 = await sendAndRecv(frame1);
        if (echoedFrame1.length < 8) throw new Error("echoed frame too short");
        const echoedPayload1 = echoedFrame1.slice(8);
        const text1 = new TextDecoder().decode(echoedPayload1);

        const payload2 = new TextEncoder().encode("hello from websocket v2");
        const frame2 = new Uint8Array(12 + payload2.length);
        frame2[0] = 0xa2;
        frame2[1] = 0x02;
        frame2[2] = 0x04;
        frame2[3] = 0x00;
        frame2[4] = (guestPort >> 8) & 0xff;
        frame2[5] = guestPort & 0xff;
        frame2.set([127, 0, 0, 1], 6);
        frame2[10] = (echoPort >> 8) & 0xff;
        frame2[11] = echoPort & 0xff;
        frame2.set(payload2, 12);

        const echoedFrame2 = await sendAndRecv(frame2);
        if (echoedFrame2.length < 12) throw new Error("echoed v2 frame too short");
        if (echoedFrame2[0] !== 0xa2 || echoedFrame2[1] !== 0x02 || echoedFrame2[2] !== 0x04 || echoedFrame2[3] !== 0x00) {
          throw new Error("echoed v2 header mismatch");
        }
        const echoedPayload2 = echoedFrame2.slice(12);
        const text2 = new TextDecoder().decode(echoedPayload2);

        ws.close();
        return { text1, text2 };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(echoed.text1).toBe("hello from websocket v1");
    expect(echoed.text2).toBe("hello from websocket v2");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("drops oversized /udp frames and increments udp_ws_dropped_oversized", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer({ MAX_DATAGRAM_PAYLOAD_BYTES: "5" });
  const web = await startWebServer();
  const droppedMetric = "udp_ws_dropped";
  const droppedOversizeMetric = "udp_ws_dropped_oversized";

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const waitForDatagram = async ({ timeoutMs }) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    return;
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  return;
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, timeoutMs);
            ws.addEventListener("message", onMessage);
          });

        const guestPort = 10_000;
        const buildFrame = (payloadText) => {
          const payload = new TextEncoder().encode(payloadText);
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);
          return frame;
        };

        // Oversized payload (10 bytes, limit 5) should be dropped without closing the socket.
        ws.send(buildFrame("0123456789"));
        const maybeOversizeResponse = await waitForDatagram({ timeoutMs: 500 });

        const okRespPromise = waitForDatagram({ timeoutMs: 10_000 });
        ws.send(buildFrame("12345"));
        const okResp = await okRespPromise;
        if (!okResp) throw new Error("timed out waiting for echoed datagram");
        if (okResp.length < 8) throw new Error("echoed frame too short");
        const okText = new TextDecoder().decode(okResp.slice(8));

        ws.close();
        return { gotOversizeResponse: maybeOversizeResponse !== null, okText };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.gotOversizeResponse).toBe(false);
    expect(res.okText).toBe("12345");

    const counters = await waitForRelayEventCounterAtLeast(relay.port, droppedOversizeMetric, 1);
    expect(getCounter(counters, droppedMetric)).toBeGreaterThanOrEqual(1);
    expect(getCounter(counters, droppedOversizeMetric)).toBeGreaterThanOrEqual(1);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("drops malformed /udp frames and increments udp_ws_dropped_malformed", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer();
  const web = await startWebServer();
  const droppedMetric = "udp_ws_dropped";
  const droppedMalformedMetric = "udp_ws_dropped_malformed";

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const waitForDatagram = async ({ timeoutMs }) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    return;
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  return;
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, timeoutMs);
            ws.addEventListener("message", onMessage);
          });

        const guestPort = 10_000;
        const buildFrame = (payloadText) => {
          const payload = new TextEncoder().encode(payloadText);
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);
          return frame;
        };

        // Malformed payload (too short to parse).
        ws.send(new Uint8Array([0x01]));
        const maybeMalformedResponse = await waitForDatagram({ timeoutMs: 500 });

        const okRespPromise = waitForDatagram({ timeoutMs: 10_000 });
        ws.send(buildFrame("hello"));
        const okResp = await okRespPromise;
        if (!okResp) throw new Error("timed out waiting for echoed datagram");
        if (okResp.length < 8) throw new Error("echoed frame too short");
        const okText = new TextDecoder().decode(okResp.slice(8));

        ws.close();
        return { gotMalformedResponse: maybeMalformedResponse !== null, okText };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.gotMalformedResponse).toBe(false);
    expect(res.okText).toBe("hello");

    const counters = await waitForRelayEventCounterAtLeast(relay.port, droppedMalformedMetric, 1);
    expect(getCounter(counters, droppedMetric)).toBeGreaterThanOrEqual(1);
    expect(getCounter(counters, droppedMalformedMetric)).toBeGreaterThanOrEqual(1);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("drops UDP replies from unexpected source ports by default (UDP_INBOUND_FILTER_MODE=address_and_port)", async ({ page }) => {
  const echo = await startUdpEchoServerDifferentSourcePort("udp4", "127.0.0.1");
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer();
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  expect(echo.replyPort).not.toBe(echo.port);

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const payload = new TextEncoder().encode("hello from websocket unexpected source port");
        const guestPort = 10_000;
        const frame = new Uint8Array(8 + payload.length);
        frame[0] = (guestPort >> 8) & 0xff;
        frame[1] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 2);
        frame[6] = (echoPort >> 8) & 0xff;
        frame[7] = echoPort & 0xff;
        frame.set(payload, 8);

        const echoedFrame = await new Promise((resolve, reject) => {
          let timeout;
          let done = false;
          const onMessage = (event) => {
            (async () => {
              let data = event.data;
              if (typeof data === "string") {
                let msg;
                try {
                  msg = JSON.parse(data);
                } catch {
                  return;
                }
                if (msg?.type === "ready") {
                  return;
                }
                if (msg?.type === "error") {
                  throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                }
                return;
              }
              if (data instanceof Blob) {
                data = await data.arrayBuffer();
              }
              if (!(data instanceof ArrayBuffer)) {
                throw new Error(`unexpected websocket message type: ${typeof data}`);
              }
              if (done) return;
              done = true;
              cleanup();
              resolve(new Uint8Array(data));
            })().catch((err) => {
              if (done) return;
              done = true;
              cleanup();
              reject(err);
            });
          };
          const cleanup = () => {
            ws.removeEventListener("message", onMessage);
            clearTimeout(timeout);
          };
          timeout = setTimeout(() => {
            if (done) return;
            done = true;
            cleanup();
            resolve(null);
          }, 1_500);
          ws.addEventListener("message", onMessage);
          ws.send(frame);
        });

        ws.close();
        return { received: echoedFrame !== null };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.received).toBe(false);

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const expectedMinDrops = beforeDrops + 1;
    const after = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, expectedMinDrops);
    expect(getCounter(after, allowlistDropMetric)).toBeGreaterThanOrEqual(expectedMinDrops);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("accepts UDP replies from unexpected source ports when UDP_INBOUND_FILTER_MODE=any", async ({ page }) => {
  const echo = await startUdpEchoServerDifferentSourcePort("udp4", "127.0.0.1");
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({ UDP_INBOUND_FILTER_MODE: "any" });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  expect(echo.replyPort).not.toBe(echo.port);

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const payload = new TextEncoder().encode("hello from websocket any inbound filter");
        const guestPort = 10_000;
        const frame = new Uint8Array(8 + payload.length);
        frame[0] = (guestPort >> 8) & 0xff;
        frame[1] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 2);
        frame[6] = (echoPort >> 8) & 0xff;
        frame[7] = echoPort & 0xff;
        frame.set(payload, 8);

        const echoedFrame = await new Promise((resolve, reject) => {
          let timeout;
          let done = false;
          const onMessage = (event) => {
            (async () => {
              let data = event.data;
              if (typeof data === "string") {
                let msg;
                try {
                  msg = JSON.parse(data);
                } catch {
                  return;
                }
                if (msg?.type === "ready") {
                  return;
                }
                if (msg?.type === "error") {
                  throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                }
                return;
              }
              if (data instanceof Blob) {
                data = await data.arrayBuffer();
              }
              if (!(data instanceof ArrayBuffer)) {
                throw new Error(`unexpected websocket message type: ${typeof data}`);
              }
              if (done) return;
              done = true;
              cleanup();
              resolve(new Uint8Array(data));
            })().catch((err) => {
              if (done) return;
              done = true;
              cleanup();
              reject(err);
            });
          };
          const cleanup = () => {
            ws.removeEventListener("message", onMessage);
            clearTimeout(timeout);
          };
          timeout = setTimeout(() => {
            if (done) return;
            done = true;
            cleanup();
            reject(new Error("timed out waiting for echoed datagram"));
          }, 10_000);
          ws.addEventListener("message", onMessage);
          ws.send(frame);
        });

        if (echoedFrame.length < 8) throw new Error("echoed frame too short");
        const remotePort = (echoedFrame[6] << 8) | echoedFrame[7];
        const echoedText = new TextDecoder().decode(echoedFrame.slice(8));

        ws.close();
        return { remotePort, echoedText };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.echoedText).toBe("hello from websocket any inbound filter");
    expect(res.remotePort).toBe(echo.replyPort);

    const counters = await waitForRelayEventCounterEquals(relay.port, allowlistDropMetric, 0);
    expect(getCounter(counters, allowlistDropMetric)).toBe(0);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("expires UDP remote allowlist entries after UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT", async ({ page }) => {
  const ttlMs = 250;
  const lateSendDelayMs = 900;
  const echo = await startUdpServerWithDelayedRepeat("udp4", "127.0.0.1", {
    delayMs: lateSendDelayMs,
    latePayload: Buffer.from("late datagram"),
  });
  test.skip(!echo, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({ UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: `${ttlMs}ms` });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const waitForDatagram = async ({ timeoutMs }) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    return;
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  return;
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, timeoutMs);
            ws.addEventListener("message", onMessage);
          });

        const payload = new TextEncoder().encode("first datagram");
        const guestPort = 10_000;
        const frame = new Uint8Array(8 + payload.length);
        frame[0] = (guestPort >> 8) & 0xff;
        frame[1] = guestPort & 0xff;
        frame.set([127, 0, 0, 1], 2);
        frame[6] = (echoPort >> 8) & 0xff;
        frame[7] = echoPort & 0xff;
        frame.set(payload, 8);

        const firstPromise = waitForDatagram({ timeoutMs: 10_000 });
        ws.send(frame);
        const first = await firstPromise;
        if (!first) throw new Error("timed out waiting for first echoed datagram");
        if (first.length < 8) throw new Error("echoed frame too short");
        const firstText = new TextDecoder().decode(first.slice(8));

        // If the allowlist entry expires, this should be dropped and we should not
        // observe a second datagram.
        // The delayed datagram is emitted ~900ms after the first send; keep a
        // conservative buffer but avoid waiting multiple seconds in the expected
        // drop case.
        const second = await waitForDatagram({ timeoutMs: 1_500 });
        const secondText = second ? new TextDecoder().decode(second.slice(8)) : null;

        ws.close();
        return { firstText, gotSecond: second !== null, secondText };
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(res.firstText).toBe("first datagram");
    if (res.gotSecond) {
      throw new Error(`expected allowlist entry to expire; received second datagram: ${res.secondText ?? "<unknown>"}`);
    }

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const expectedMinDrops = beforeDrops + 1;
    const after = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, expectedMinDrops, { timeoutMs: 8_000 });
    expect(getCounter(after, allowlistDropMetric)).toBeGreaterThanOrEqual(expectedMinDrops);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("evicts UDP remote allowlist entries when MAX_ALLOWED_REMOTES_PER_BINDING is exceeded", async ({ page }) => {
  const echoA = await startUdpServerWithDelayedRepeat("udp4", "127.0.0.1", {
    delayMs: 1_000,
    latePayload: Buffer.from("late from A"),
  });
  const echoB = await startUdpEchoServer("udp4", "127.0.0.1");
  test.skip(!echoA || !echoB, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({
    MAX_ALLOWED_REMOTES_PER_BINDING: "1",
    UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: "10s",
  });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  const allowlistEvictMetric = "udp_remote_allowlist_evictions_total";

  try {
    await page.goto(web.url);

    const before = await getRelayEventCounters(relay.port);

    const res = await page.evaluate(
      async ({ relayPort, echoPortA, echoPortB }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const waitForDatagram = async ({ timeoutMs }) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    return;
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  return;
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, timeoutMs);
            ws.addEventListener("message", onMessage);
          });

        const guestPort = 10_000;
        const buildFrame = (remotePort, text) => {
          const payload = new TextEncoder().encode(text);
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (remotePort >> 8) & 0xff;
          frame[7] = remotePort & 0xff;
          frame.set(payload, 8);
          return frame;
        };

        const sendAndRecvText = async (remotePort, text) => {
          const frame = buildFrame(remotePort, text);
          const respPromise = waitForDatagram({ timeoutMs: 10_000 });
          ws.send(frame);
          const resp = await respPromise;
          if (!resp) throw new Error(`timed out waiting for datagram response for ${text}`);
          if (resp.length < 8) throw new Error("echoed frame too short");
          return new TextDecoder().decode(resp.slice(8));
        };

        const textA = await sendAndRecvText(echoPortA, "hello A");
        // MAX_ALLOWED_REMOTES_PER_BINDING=1 should evict the first remote once we
        // talk to a second destination on the same guest port binding.
        const textB = await sendAndRecvText(echoPortB, "hello B");

        // A now sends a delayed datagram. If eviction worked, we should not see it.
        // The delayed datagram is emitted ~1s after the first send; keep a
        // conservative buffer but avoid waiting too long in the expected drop
        // case.
        const late = await waitForDatagram({ timeoutMs: 1_800 });
        const lateText = late ? new TextDecoder().decode(late.slice(8)) : null;

        ws.close();
        return { textA, textB, gotLate: late !== null, lateText };
      },
      { relayPort: relay.port, echoPortA: echoA.port, echoPortB: echoB.port },
    );

    expect(res.textA).toBe("hello A");
    expect(res.textB).toBe("hello B");
    if (res.gotLate) {
      throw new Error(`expected allowlist entry A to be evicted; received datagram: ${res.lateText ?? "<unknown>"}`);
    }

    const beforeEvictions = getCounter(before, allowlistEvictMetric);
    const afterEvictions = await waitForRelayEventCounterAtLeast(relay.port, allowlistEvictMetric, beforeEvictions + 1);
    expect(getCounter(afterEvictions, allowlistEvictMetric)).toBeGreaterThanOrEqual(beforeEvictions + 1);

    const beforeDrops = getCounter(before, allowlistDropMetric);
    const afterDrops = await waitForRelayEventCounterAtLeast(relay.port, allowlistDropMetric, beforeDrops + 1, { timeoutMs: 8_000 });
    expect(getCounter(afterDrops, allowlistDropMetric)).toBeGreaterThanOrEqual(beforeDrops + 1);
  } finally {
    await Promise.all([web.close(), relay.kill(), echoA?.close(), echoB?.close()]);
  }
});

test("treats MAX_ALLOWED_REMOTES_PER_BINDING as a per-guest-port limit (not per-session)", async ({ page }) => {
  const echoA = await startUdpServerWithDelayedRepeat("udp4", "127.0.0.1", {
    delayMs: 1_200,
    latePayload: Buffer.from("late from A"),
  });
  const echoB = await startUdpEchoServer("udp4", "127.0.0.1");
  test.skip(!echoA || !echoB, "udp4 not supported in test environment");
  const relay = await spawnRelayServer({
    MAX_ALLOWED_REMOTES_PER_BINDING: "1",
    UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT: "10s",
  });
  const web = await startWebServer();
  const allowlistDropMetric = "udp_remote_allowlist_overflow_drops_total";
  const allowlistEvictMetric = "udp_remote_allowlist_evictions_total";

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPortA, echoPortB }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const waitForDatagram = async ({ timeoutMs }) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    return;
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  return;
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              resolve(null);
            }, timeoutMs);
            ws.addEventListener("message", onMessage);
          });

        const buildFrame = (guestPort, remotePort, text) => {
          const payload = new TextEncoder().encode(text);
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (remotePort >> 8) & 0xff;
          frame[7] = remotePort & 0xff;
          frame.set(payload, 8);
          return frame;
        };

        const sendAndRecvText = async (guestPort, remotePort, text) => {
          const frame = buildFrame(guestPort, remotePort, text);
          const respPromise = waitForDatagram({ timeoutMs: 10_000 });
          ws.send(frame);
          const resp = await respPromise;
          if (!resp) throw new Error(`timed out waiting for echoed datagram for ${text}`);
          if (resp.length < 8) throw new Error("echoed frame too short");
          const gotText = new TextDecoder().decode(resp.slice(8));
          const gotGuestPort = (resp[0] << 8) | resp[1];
          const gotRemotePort = (resp[6] << 8) | resp[7];
          return { gotText, gotGuestPort, gotRemotePort };
        };

        // Use two different guest ports so each gets its own allowlist/binding.
        const guestPortA = 10_000;
        const guestPortB = 10_001;

        const respB = await sendAndRecvText(guestPortB, echoPortB, "hello B");
        // Send to A last so the delayed datagram cannot race with the B response.
        const respA = await sendAndRecvText(guestPortA, echoPortA, "hello A");

        const late = await waitForDatagram({ timeoutMs: 4_000 });
        if (!late) throw new Error("timed out waiting for delayed datagram");
        if (late.length < 8) throw new Error("late frame too short");
        const lateGuestPort = (late[0] << 8) | late[1];
        const lateRemotePort = (late[6] << 8) | late[7];
        const lateText = new TextDecoder().decode(late.slice(8));

        ws.close();
        return { respA, respB, lateGuestPort, lateRemotePort, lateText };
      },
      { relayPort: relay.port, echoPortA: echoA.port, echoPortB: echoB.port },
    );

    expect(res.respA.gotText).toBe("hello A");
    expect(res.respA.gotGuestPort).toBe(10_000);
    expect(res.respA.gotRemotePort).toBe(echoA.port);

    expect(res.respB.gotText).toBe("hello B");
    expect(res.respB.gotGuestPort).toBe(10_001);
    expect(res.respB.gotRemotePort).toBe(echoB.port);

    expect(res.lateText).toBe("late from A");
    expect(res.lateGuestPort).toBe(10_000);
    expect(res.lateRemotePort).toBe(echoA.port);

    const counters = await waitForRelayEventCounterEquals(relay.port, allowlistEvictMetric, 0);
    expect(getCounter(counters, allowlistEvictMetric)).toBe(0);
    expect(getCounter(counters, allowlistDropMetric)).toBe(0);
  } finally {
    await Promise.all([web.close(), relay.kill(), echoA?.close(), echoB?.close()]);
  }
});

test("relays UDP datagrams to an IPv6 destination via the /udp WebSocket fallback (v2)", async ({ page }) => {
  const echo = await startUdpEchoServer("udp6", "::1");
  test.skip(!echo, "ipv6 not supported in test environment");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        ws.binaryType = "arraybuffer";
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const payload = new TextEncoder().encode("hello from websocket ipv6");
        const guestPort = 10_000;

        const frame = new Uint8Array(UDP_RELAY_V2_IPV6_HEADER_BYTES + payload.length);
        frame[0] = 0xa2;
        frame[1] = 0x02;
        frame[2] = 0x06;
        frame[3] = 0x00;
        frame[UDP_RELAY_V2_GUEST_PORT_OFFSET] = (guestPort >> 8) & 0xff;
        frame[UDP_RELAY_V2_GUEST_PORT_OFFSET + 1] = guestPort & 0xff;
        // ::1
        frame[UDP_RELAY_V2_REMOTE_IP_OFFSET + UDP_RELAY_V2_IPV6_ADDR_BYTES - 1] = 1;
        frame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6] = (echoPort >> 8) & 0xff;
        frame[UDP_RELAY_V2_REMOTE_PORT_OFFSET_IPV6 + 1] = echoPort & 0xff;
        frame.set(payload, UDP_RELAY_V2_IPV6_HEADER_BYTES);

        const sendAndRecv = async (frame) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    throw new Error(`unexpected websocket text message: ${data}`);
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  throw new Error(`unexpected websocket text message: ${data}`);
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              reject(new Error("timed out waiting for echoed datagram"));
            }, 10_000);
            ws.addEventListener("message", onMessage);
            ws.send(frame);
          });

        const echoedFrame = await sendAndRecv(frame);

        if (echoedFrame.length < UDP_RELAY_V2_IPV6_HEADER_BYTES) throw new Error("echoed frame too short");
        if (echoedFrame[0] !== 0xa2 || echoedFrame[1] !== 0x02 || echoedFrame[2] !== 0x06 || echoedFrame[3] !== 0x00) {
          throw new Error("v2 header mismatch");
        }

        const echoedPayload = echoedFrame.slice(UDP_RELAY_V2_IPV6_HEADER_BYTES);
        const echoedText = new TextDecoder().decode(echoedPayload);
        ws.close();
        return echoedText;
      },
      { relayPort: relay.port, echoPort: echo.port },
    );

    expect(echoed).toBe("hello from websocket ipv6");
  } finally {
    await Promise.all([web.close(), relay.kill(), echo?.close()]);
  }
});

test("authenticates /udp via JWT (query-string + first message handshake)", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);
  const token = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const invalidToken = mintHS256JWT({
    sid: "sess_e2e",
    iat: now,
    exp: now + 5 * 60,
    secret: `${jwtSecret}-wrong`,
  });

  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, echoPort, token, invalidToken }) => {
        const waitForOpen = (ws) =>
          new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

        const waitForReady = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for ready")), 10_000);
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`unexpected websocket text message: ${event.data}`));
                return;
              }
              if (msg?.type === "ready") {
                if (typeof msg.sessionId !== "string" || msg.sessionId.length === 0) {
                  clearTimeout(timeout);
                  ws.removeEventListener("message", onMessage);
                  reject(new Error("ready message missing sessionId"));
                  return;
                }
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                resolve(msg);
                return;
              }
              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }
            };
            ws.addEventListener("message", onMessage);
          });

        const sendAndRecv = async (ws, frame) =>
          await new Promise((resolve, reject) => {
            let timeout;
            let done = false;
            const onMessage = (event) => {
              (async () => {
                let data = event.data;
                if (typeof data === "string") {
                  let msg;
                  try {
                    msg = JSON.parse(data);
                  } catch {
                    throw new Error(`unexpected websocket text message: ${data}`);
                  }
                  if (msg?.type === "ready") {
                    return;
                  }
                  if (msg?.type === "error") {
                    throw new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`);
                  }
                  throw new Error(`unexpected websocket text message: ${data}`);
                }
                if (data instanceof Blob) {
                  data = await data.arrayBuffer();
                }
                if (!(data instanceof ArrayBuffer)) {
                  throw new Error(`unexpected websocket message type: ${typeof data}`);
                }
                if (done) return;
                done = true;
                cleanup();
                resolve(new Uint8Array(data));
              })().catch((err) => {
                if (done) return;
                done = true;
                cleanup();
                reject(err);
              });
            };
            const cleanup = () => {
              ws.removeEventListener("message", onMessage);
              clearTimeout(timeout);
            };
            timeout = setTimeout(() => {
              if (done) return;
              done = true;
              cleanup();
              reject(new Error("timed out waiting for echoed datagram"));
            }, 10_000);
            ws.addEventListener("message", onMessage);
            ws.send(frame);
          });

        const waitForErrorAndClose = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for unauthorized close")), 10_000);
            let errMsg;
            let closed;
            const maybeDone = () => {
              if (!errMsg || !closed) return;
              cleanup();
              resolve({ errMsg, closeCode: closed.code, closeReason: closed.reason });
            };
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                return;
              }
              if (msg?.type === "error") {
                errMsg = msg;
                maybeDone();
              }
            };
            const onClose = (event) => {
              closed = event;
              maybeDone();
            };
            const cleanup = () => {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              ws.removeEventListener("close", onClose);
            };
            ws.addEventListener("message", onMessage);
            ws.addEventListener("close", onClose);
          });

        const guestPort = 10_000;
        const buildV1Frame = (text) => {
          const payload = new TextEncoder().encode(text);
          const frame = new Uint8Array(8 + payload.length);
          frame[0] = (guestPort >> 8) & 0xff;
          frame[1] = guestPort & 0xff;
          frame.set([127, 0, 0, 1], 2);
          frame[6] = (echoPort >> 8) & 0xff;
          frame[7] = echoPort & 0xff;
          frame.set(payload, 8);
          return frame;
        };

        const queryText = "hello from websocket jwt query";
        const wsQuery = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?token=${encodeURIComponent(token)}`);
        wsQuery.binaryType = "arraybuffer";
        const queryReadyPromise = waitForReady(wsQuery);
        await waitForOpen(wsQuery);
        await queryReadyPromise;
        const echoedQueryFrame = await sendAndRecv(wsQuery, buildV1Frame(queryText));
        wsQuery.close();
        const echoedQueryText = new TextDecoder().decode(echoedQueryFrame.slice(8));

        const apiKeyQueryText = "hello from websocket jwt apiKey query";
        const wsAPIKeyQuery = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?apiKey=${encodeURIComponent(token)}`);
        wsAPIKeyQuery.binaryType = "arraybuffer";
        const apiKeyReadyPromise = waitForReady(wsAPIKeyQuery);
        await waitForOpen(wsAPIKeyQuery);
        await apiKeyReadyPromise;
        const echoedAPIKeyFrame = await sendAndRecv(wsAPIKeyQuery, buildV1Frame(apiKeyQueryText));
        wsAPIKeyQuery.close();
        const echoedAPIKeyText = new TextDecoder().decode(echoedAPIKeyFrame.slice(8));

        const firstMsgText = "hello from websocket jwt first message";
        const wsAuthMsg = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        wsAuthMsg.binaryType = "arraybuffer";
        const authReadyPromise = waitForReady(wsAuthMsg);
        await waitForOpen(wsAuthMsg);
        wsAuthMsg.send(JSON.stringify({ type: "auth", token }));
        await authReadyPromise;
        const echoedFirstMsgFrame = await sendAndRecv(wsAuthMsg, buildV1Frame(firstMsgText));
        wsAuthMsg.close();
        const echoedFirstMsgText = new TextDecoder().decode(echoedFirstMsgFrame.slice(8));

        const firstMsgAPIKeyText = "hello from websocket jwt first message apiKey";
        const wsAuthAPIKeyMsg = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        wsAuthAPIKeyMsg.binaryType = "arraybuffer";
        const authAPIKeyReadyPromise = waitForReady(wsAuthAPIKeyMsg);
        await waitForOpen(wsAuthAPIKeyMsg);
        wsAuthAPIKeyMsg.send(JSON.stringify({ type: "auth", apiKey: token }));
        await authAPIKeyReadyPromise;
        const echoedFirstMsgAPIKeyFrame = await sendAndRecv(wsAuthAPIKeyMsg, buildV1Frame(firstMsgAPIKeyText));
        wsAuthAPIKeyMsg.close();
        const echoedFirstMsgAPIKeyText = new TextDecoder().decode(echoedFirstMsgAPIKeyFrame.slice(8));

        const wsInvalidQuery = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?token=${encodeURIComponent(invalidToken)}`);
        wsInvalidQuery.binaryType = "arraybuffer";
        const invalidQueryPromise = waitForErrorAndClose(wsInvalidQuery);
        await waitForOpen(wsInvalidQuery);
        const invalidQueryRes = await invalidQueryPromise;

        // Sending datagrams before completing auth (and before receiving ready) should be rejected.
        const wsMissing = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        wsMissing.binaryType = "arraybuffer";
        const missingPromise = waitForErrorAndClose(wsMissing);
        await waitForOpen(wsMissing);
        wsMissing.send(buildV1Frame("should be rejected"));
        const missingRes = await missingPromise;

        const wsInvalid = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        wsInvalid.binaryType = "arraybuffer";
        const invalidPromise = waitForErrorAndClose(wsInvalid);
        await waitForOpen(wsInvalid);
        wsInvalid.send(JSON.stringify({ type: "auth", token: `${token}-invalid` }));
        const invalidRes = await invalidPromise;

        const wsMismatch = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        wsMismatch.binaryType = "arraybuffer";
        const mismatchPromise = waitForErrorAndClose(wsMismatch);
        await waitForOpen(wsMismatch);
        wsMismatch.send(JSON.stringify({ type: "auth", token, apiKey: `${token}-mismatch` }));
        const mismatchRes = await mismatchPromise;

        return {
          echoedQueryText,
          echoedAPIKeyText,
          echoedFirstMsgText,
          echoedFirstMsgAPIKeyText,
          invalidQueryRes,
          missingRes,
          invalidRes,
          mismatchRes,
        };
      },
      { relayPort: relay.port, echoPort: echo.port, token, invalidToken },
    );

    expect(res.echoedQueryText).toBe("hello from websocket jwt query");
    expect(res.echoedAPIKeyText).toBe("hello from websocket jwt apiKey query");
    expect(res.echoedFirstMsgText).toBe("hello from websocket jwt first message");
    expect(res.echoedFirstMsgAPIKeyText).toBe("hello from websocket jwt first message apiKey");

    expect(res.invalidQueryRes.errMsg?.code).toBe("unauthorized");
    expect(res.invalidQueryRes.closeCode).toBe(1008);

    expect(res.missingRes.errMsg?.code).toBe("unauthorized");
    expect(res.missingRes.closeCode).toBe(1008);

    expect(res.invalidRes.errMsg?.code).toBe("unauthorized");
    expect(res.invalidRes.closeCode).toBe(1008);

    expect(res.mismatchRes.errMsg?.code).toBe("bad_message");
    expect(res.mismatchRes.closeCode).toBe(1008);
  } finally {
    await Promise.all([web.close(), relay.kill(), echo.close()]);
  }
});

test("rejects concurrent /udp WebSocket sessions with the same JWT sid", async ({ page }) => {
  const jwtSecret = "e2e-jwt-secret";
  const now = Math.floor(Date.now() / 1000);

  const queryTokenA = mintHS256JWT({
    sid: "sess_e2e_udp_query",
    iat: now - 10,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const queryTokenB = mintHS256JWT({
    sid: "sess_e2e_udp_query",
    iat: now - 9,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const authMsgTokenA = mintHS256JWT({
    sid: "sess_e2e_udp_auth_msg",
    iat: now - 10,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });
  const authMsgTokenB = mintHS256JWT({
    sid: "sess_e2e_udp_auth_msg",
    iat: now - 9,
    exp: now + 5 * 60,
    secret: jwtSecret,
  });

  const relay = await spawnRelayServer({
    AUTH_MODE: "jwt",
    JWT_SECRET: jwtSecret,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort, queryTokenA, queryTokenB, authMsgTokenA, authMsgTokenB }) => {
        const waitForOpen = (ws) =>
          new Promise((resolve, reject) => {
            ws.addEventListener("open", () => resolve(), { once: true });
            ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
          });

        const waitForClosed = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for websocket close")), 10_000);
            ws.addEventListener(
              "close",
              (event) => {
                clearTimeout(timeout);
                resolve({ code: event.code, reason: event.reason });
              },
              { once: true },
            );
          });

        const waitForReady = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for ready")), 10_000);
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`unexpected websocket text message: ${event.data}`));
                return;
              }
              if (msg?.type === "ready") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                resolve(msg);
                return;
              }
              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`udp websocket error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }
            };
            ws.addEventListener("message", onMessage);
          });

        const waitForErrorAndClose = async (ws) =>
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for error + close")), 10_000);
            let errMsg;
            let closed;
            const maybeDone = () => {
              if (!errMsg || !closed) return;
              cleanup();
              resolve({ errMsg, closeCode: closed.code, closeReason: closed.reason });
            };
            const onMessage = (event) => {
              if (typeof event.data !== "string") return;
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                return;
              }
              if (msg?.type === "error") {
                errMsg = msg;
                maybeDone();
              }
            };
            const onClose = (event) => {
              closed = event;
              maybeDone();
            };
            const cleanup = () => {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              ws.removeEventListener("close", onClose);
            };
            ws.addEventListener("message", onMessage);
            ws.addEventListener("close", onClose);
          });

        // Query-string auth path.
        const ws1 = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?token=${encodeURIComponent(queryTokenA)}`);
        const ready1Promise = waitForReady(ws1);
        await waitForOpen(ws1);
        await ready1Promise;

        const ws2 = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?token=${encodeURIComponent(queryTokenB)}`);
        const err2Promise = waitForErrorAndClose(ws2);
        await waitForOpen(ws2);
        const err2 = await err2Promise;

        const ws1ClosedPromise = waitForClosed(ws1);
        ws1.close();
        await ws1ClosedPromise;

        // After the first session ends, the stable `sid` key should be reusable.
        const ws1b = new WebSocket(`ws://127.0.0.1:${relayPort}/udp?token=${encodeURIComponent(queryTokenB)}`);
        const ready1bPromise = waitForReady(ws1b);
        await waitForOpen(ws1b);
        await ready1bPromise;
        const ws1bClosedPromise = waitForClosed(ws1b);
        ws1b.close();
        await ws1bClosedPromise;

        // First-message auth path.
        const ws3 = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        const ready3Promise = waitForReady(ws3);
        await waitForOpen(ws3);
        ws3.send(JSON.stringify({ type: "auth", token: authMsgTokenA }));
        await ready3Promise;

        const ws4 = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        const err4Promise = waitForErrorAndClose(ws4);
        await waitForOpen(ws4);
        ws4.send(JSON.stringify({ type: "auth", token: authMsgTokenB }));
        const err4 = await err4Promise;

        const ws3ClosedPromise = waitForClosed(ws3);
        ws3.close();
        await ws3ClosedPromise;

        const ws3b = new WebSocket(`ws://127.0.0.1:${relayPort}/udp`);
        const ready3bPromise = waitForReady(ws3b);
        await waitForOpen(ws3b);
        ws3b.send(JSON.stringify({ type: "auth", token: authMsgTokenB }));
        await ready3bPromise;
        const ws3bClosedPromise = waitForClosed(ws3b);
        ws3b.close();
        await ws3bClosedPromise;

        return { err2, err4 };
      },
      { relayPort: relay.port, queryTokenA, queryTokenB, authMsgTokenA, authMsgTokenB },
    );

    expect(res.err2.errMsg?.code).toBe("session_already_active");
    expect(res.err2.closeCode).toBe(1013);
    expect(["session_already_active", "session already active", ""]).toContain(res.err2.closeReason ?? "");

    expect(res.err4.errMsg?.code).toBe("session_already_active");
    expect(res.err4.closeCode).toBe(1013);
    expect(["session_already_active", "session already active", ""]).toContain(res.err4.closeReason ?? "");
  } finally {
    await Promise.all([web.close(), relay.kill()]);
  }
});

test("bridges an L2 tunnel DataChannel to a backend WebSocket", async ({ page }) => {
  const origin = "https://example.com";
  const token = "e2e-token";
  const backend = await spawnL2BackendServer({
    REQUIRE_ORIGIN: origin,
    REQUIRE_TOKEN: token,
  });
  const relay = await spawnRelayServer({
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_WS_ORIGIN: origin,
    L2_BACKEND_WS_TOKEN: token,
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const pong = await page.evaluate(
      async ({ relayPort }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        // L2 tunnel MUST be reliable (no partial reliability) and ordered. Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: true });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        // PING per docs/l2-tunnel-protocol.md: magic (0xA2) + ver (0x03) + type (0x01) + flags (0).
        dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00]));

        const res = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for PONG")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        ws.close();
        pc.close();
        return Array.from(res);
      },
      { relayPort: relay.port },
    );

    expect(pong).toEqual([0xa2, 0x03, 0x02, 0x00]); // PONG

    const debug = await page.request.get(`http://127.0.0.1:${backend.port}/debug`);
    expect(debug.ok()).toBeTruthy();
    const debugJSON = await debug.json();
    expect(debugJSON.origin).toBe(origin);
    expect(debugJSON.token).toBe(token);
    expect(debugJSON.tokenSource).toBe("subprotocol");

    const metricsResp = await page.request.get(`http://127.0.0.1:${relay.port}/metrics`);
    expect(metricsResp.ok()).toBeTruthy();
    const events = parseRelayEventCounters(await metricsResp.text());
    expect(events.l2_bridge_dials_total).toBeGreaterThanOrEqual(1);
    expect(events.l2_bridge_dial_errors_total ?? 0).toBe(0);
    expect(events.l2_bridge_messages_from_client_total).toBeGreaterThanOrEqual(1);
    expect(events.l2_bridge_messages_to_client_total).toBeGreaterThanOrEqual(1);
    expect(events.l2_bridge_bytes_from_client_total).toBeGreaterThanOrEqual(4);
    expect(events.l2_bridge_bytes_to_client_total).toBeGreaterThanOrEqual(4);
    expect(events.l2_bridge_dropped_oversized_total ?? 0).toBe(0);
    expect(events.l2_bridge_dropped_rate_limited_total ?? 0).toBe(0);
  } finally {
    await Promise.all([web.close(), relay.kill(), backend.kill()]);
  }
});

test("drops oversized L2 tunnel messages and increments metric", async ({ page }) => {
  const origin = "https://example.com";
  const token = "e2e-token";
  const backend = await spawnL2BackendServer({
    REQUIRE_ORIGIN: origin,
    REQUIRE_TOKEN: token,
  });
  // Use a tiny L2_MAX_MESSAGE_BYTES so we can trigger the oversize path with a
  // small message (avoids large allocations in tests).
  const relay = await spawnRelayServer({
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_WS_ORIGIN: origin,
    L2_BACKEND_WS_TOKEN: token,
    L2_MAX_MESSAGE_BYTES: "4",
  });
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const res = await page.evaluate(
      async ({ relayPort }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        // L2 tunnel MUST be reliable (no partial reliability) and ordered. Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: true });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        // First validate the bridge works with an in-limit ping.
        dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00])); // PING

        const pong = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for PONG")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        // Now send an oversized message (5 bytes > L2_MAX_MESSAGE_BYTES=4) and
        // ensure the relay closes the channel.
        const closeRes = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel close")), 10_000);
          dc.addEventListener(
            "close",
            () => {
              clearTimeout(timeout);
              resolve(true);
            },
            { once: true },
          );
          dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00, 0x00]));
        });

        ws.close();
        pc.close();

        return { pong: Array.from(pong), closed: closeRes };
      },
      { relayPort: relay.port },
    );

    expect(res.pong).toEqual([0xa2, 0x03, 0x02, 0x00]); // PONG
    expect(res.closed).toBe(true);

    const metricsResp = await page.request.get(`http://127.0.0.1:${relay.port}/metrics`);
    expect(metricsResp.ok()).toBeTruthy();
    const events = parseRelayEventCounters(await metricsResp.text());
    expect(events.l2_bridge_dials_total).toBeGreaterThanOrEqual(1);
    expect(events.l2_bridge_dial_errors_total ?? 0).toBe(0);
    expect(events.l2_bridge_dropped_oversized_total).toBeGreaterThanOrEqual(1);
  } finally {
    await Promise.all([web.close(), relay.kill(), backend.kill()]);
  }
});

test("bridges an L2 tunnel DataChannel to a backend WebSocket (session cookie forwarding)", async ({ page }) => {
  const web = await startWebServer();
  const requiredCookieValue = "test-session-cookie";
  const requiredOrigin = new URL(web.url).origin;
  const backend = await spawnL2BackendServer({
    REQUIRE_COOKIE_NAME: "aero_session",
    REQUIRE_COOKIE_VALUE: requiredCookieValue,
    REQUIRE_ORIGIN: requiredOrigin,
  });

  // Cookie must be set before WebSocket signaling so the relay can bind the L2
  // backend dial to the caller's session.
  await page.context().addCookies([
    {
      url: "http://127.0.0.1/",
      name: "aero_session",
      value: requiredCookieValue,
    },
  ]);

  const relayWithCookie = await spawnRelayServer({
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_FORWARD_AERO_SESSION: "1",
    // Forward the browser Origin by default; set explicitly to keep the test deterministic.
    L2_BACKEND_FORWARD_ORIGIN: "1",
  });
  const relayWithoutCookie = await spawnRelayServer({
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_FORWARD_ORIGIN: "1",
  });

  const runPing = async (relayPort, expectPong) =>
    await page.evaluate(
      async ({ relayPort, expectPong }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        // L2 tunnel MUST be reliable (no partial reliability) and ordered. Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: true });
        dc.binaryType = "arraybuffer";

        try {
          const answerPromise = new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
            let answered = false;
            const onMessage = (event) => {
              let msg;
              try {
                msg = JSON.parse(event.data);
              } catch {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error("invalid signaling message (not JSON)"));
                return;
              }

              if (msg?.type === "error") {
                clearTimeout(timeout);
                ws.removeEventListener("message", onMessage);
                reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
                return;
              }

              if (msg?.type === "candidate") {
                if (!msg.candidate?.candidate) return;
                if (remoteDescriptionSet) {
                  pc.addIceCandidate(msg.candidate).catch(() => {});
                } else {
                  pendingCandidates.push(msg.candidate);
                }
                return;
              }

              if (msg?.type !== "answer") return;
              if (answered) return;
              answered = true;
              clearTimeout(timeout);
              resolve(msg);
            };
            ws.addEventListener("message", onMessage);
          });

          const offer = await pc.createOffer();
          await pc.setLocalDescription(offer);

          await new Promise((resolve) => {
            if (pc.iceGatheringState === "complete") return resolve();
            const onState = () => {
              if (pc.iceGatheringState !== "complete") return;
              pc.removeEventListener("icegatheringstatechange", onState);
              resolve();
            };
            pc.addEventListener("icegatheringstatechange", onState);
          });

          if (!pc.localDescription?.sdp) {
            throw new Error("missing local description");
          }

          ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

          const answerMsg = await answerPromise;
          if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
            throw new Error("invalid answer message shape");
          }

          await pc.setRemoteDescription(answerMsg.sdp);
          remoteDescriptionSet = true;
          for (const candidate of pendingCandidates) {
            await pc.addIceCandidate(candidate);
          }

          let dcState = dc.readyState;
          if (dcState !== "open" && dcState !== "closed") {
            dcState = await new Promise((resolve, reject) => {
              const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel state")), 10_000);
              const cleanup = () => {
                clearTimeout(timeout);
                dc.removeEventListener("open", onOpen);
                dc.removeEventListener("close", onClose);
                dc.removeEventListener("error", onError);
              };
              const onOpen = () => {
                cleanup();
                resolve("open");
              };
              const onClose = () => {
                cleanup();
                resolve("close");
              };
              const onError = () => {
                cleanup();
                resolve("error");
              };
              dc.addEventListener("open", onOpen, { once: true });
              dc.addEventListener("close", onClose, { once: true });
              dc.addEventListener("error", onError, { once: true });
            });
          }

          if (dcState !== "open") {
            return { status: dcState };
          }

          // PING per docs/l2-tunnel-protocol.md: magic (0xA2) + ver (0x03) + type (0x01) + flags (0).
          dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00]));

          const res = await new Promise((resolve) => {
            const timeoutMs = expectPong ? 10_000 : 5_000;
            const timeout = setTimeout(() => resolve({ status: "timeout" }), timeoutMs);
            dc.addEventListener(
              "message",
              (event) => {
                clearTimeout(timeout);
                resolve({ status: "message", data: Array.from(new Uint8Array(event.data)) });
              },
              { once: true },
            );
            dc.addEventListener(
              "close",
              () => {
                clearTimeout(timeout);
                resolve({ status: "close" });
              },
              { once: true },
            );
          });

          if (!expectPong) return res;
          if (res.status !== "message") throw new Error(`expected message, got ${res.status}`);
          return res;
        } finally {
          ws.close();
          pc.close();
        }
      },
      { relayPort, expectPong },
    );

  try {
    await page.goto(web.url);

    const pong = await runPing(relayWithCookie.port, true);
    expect(pong.data).toEqual([0xa2, 0x03, 0x02, 0x00]); // PONG

    const metricsWithCookie = await page.request.get(`http://127.0.0.1:${relayWithCookie.port}/metrics`);
    expect(metricsWithCookie.ok()).toBeTruthy();
    const eventsWithCookie = parseRelayEventCounters(await metricsWithCookie.text());
    expect(eventsWithCookie.l2_bridge_dials_total).toBeGreaterThanOrEqual(1);
    expect(eventsWithCookie.l2_bridge_dial_errors_total ?? 0).toBe(0);

    const failure = await runPing(relayWithoutCookie.port, false);
    expect(failure.status).not.toBe("message");

    const metricsWithoutCookie = await page.request.get(`http://127.0.0.1:${relayWithoutCookie.port}/metrics`);
    expect(metricsWithoutCookie.ok()).toBeTruthy();
    const eventsWithoutCookie = parseRelayEventCounters(await metricsWithoutCookie.text());
    expect(eventsWithoutCookie.l2_bridge_dials_total).toBeGreaterThanOrEqual(1);
    expect(eventsWithoutCookie.l2_bridge_dial_errors_total).toBeGreaterThanOrEqual(1);
  } finally {
    await Promise.all([web.close(), relayWithCookie.kill(), relayWithoutCookie.kill(), backend.kill()]);
  }
});

test("forwards client Origin + auth credential when bridging an L2 tunnel (query)", async ({ page }) => {
  const apiKey = "e2e-credential";
  const web = await startWebServer();
  const expectedOrigin = new URL(web.url).origin;
  const backend = await spawnL2BackendServer({
    REQUIRE_ORIGIN: expectedOrigin,
    REQUIRE_TOKEN: apiKey,
  });
  const relay = await spawnRelayServer({
    AUTH_MODE: "api_key",
    API_KEY: apiKey,
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_AUTH_FORWARD_MODE: "query",
    // Forward the browser Origin by default; set explicitly to keep the test deterministic.
    L2_BACKEND_FORWARD_ORIGIN: "1",
    // Ensure we test the per-session forwarded credential, not a fixed backend token.
    L2_BACKEND_WS_TOKEN: "",
    L2_BACKEND_WS_ORIGIN: "",
    L2_BACKEND_ORIGIN_OVERRIDE: "",
  });

  try {
    await page.goto(web.url);

    const pong = await page.evaluate(
      async ({ relayPort, apiKey }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: {
            "X-API-Key": apiKey,
          },
        }).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        // WebSocket upgrade requests cannot include arbitrary headers, so we
        // authenticate using the first control-plane message.
        ws.send(JSON.stringify({ type: "auth", apiKey }));

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        // L2 tunnel MUST be reliable (no partial reliability) and ordered. Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: true });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        // PING per docs/l2-tunnel-protocol.md: magic (0xA2) + ver (0x03) + type (0x01) + flags (0).
        dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00]));

        const res = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for PONG")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        ws.close();
        pc.close();
        return Array.from(res);
      },
      { relayPort: relay.port, apiKey },
    );

    expect(pong).toEqual([0xa2, 0x03, 0x02, 0x00]); // PONG

    const debug = await page.request.get(`http://127.0.0.1:${backend.port}/debug`);
    expect(debug.ok()).toBeTruthy();
    const debugJSON = await debug.json();
    expect(debugJSON.origin).toBe(expectedOrigin);
    expect(debugJSON.token).toBe(apiKey);
    expect(debugJSON.tokenSource).toBe("query");
  } finally {
    await Promise.all([web.close(), relay.kill(), backend.kill()]);
  }
});

test("forwards client Origin + auth credential when bridging an L2 tunnel (subprotocol)", async ({ page }) => {
  const apiKey = "e2e-credential";
  const web = await startWebServer();
  const expectedOrigin = new URL(web.url).origin;
  const backend = await spawnL2BackendServer({
    REQUIRE_ORIGIN: expectedOrigin,
    REQUIRE_TOKEN: apiKey,
  });
  const relay = await spawnRelayServer({
    AUTH_MODE: "api_key",
    API_KEY: apiKey,
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
    L2_BACKEND_AUTH_FORWARD_MODE: "subprotocol",
    // Forward the browser Origin by default; set explicitly to keep the test deterministic.
    L2_BACKEND_FORWARD_ORIGIN: "1",
    // Ensure we test the per-session forwarded credential, not a fixed backend token.
    L2_BACKEND_WS_TOKEN: "",
    L2_BACKEND_WS_ORIGIN: "",
    L2_BACKEND_ORIGIN_OVERRIDE: "",
  });

  try {
    await page.goto(web.url);

    const pong = await page.evaluate(
      async ({ relayPort, apiKey }) => {
        const iceResp = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`, {
          headers: {
            "X-API-Key": apiKey,
          },
        }).then((r) => r.json());
        if (!iceResp?.iceServers || !Array.isArray(iceResp.iceServers)) {
          throw new Error("invalid ice server response");
        }
        const iceServers = iceResp.iceServers;

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        // WebSocket upgrade requests cannot include arbitrary headers, so we
        // authenticate using the first control-plane message.
        ws.send(JSON.stringify({ type: "auth", apiKey }));

        const pc = new RTCPeerConnection({ iceServers });
        const pendingCandidates = [];
        let remoteDescriptionSet = false;
        // L2 tunnel MUST be reliable (no partial reliability) and ordered. Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: true });
        dc.binaryType = "arraybuffer";

        const answerPromise = new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          let answered = false;
          const onMessage = (event) => {
            let msg;
            try {
              msg = JSON.parse(event.data);
            } catch {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error("invalid signaling message (not JSON)"));
              return;
            }

            if (msg?.type === "error") {
              clearTimeout(timeout);
              ws.removeEventListener("message", onMessage);
              reject(new Error(`signaling error: ${msg.code ?? "unknown"}: ${msg.message ?? ""}`));
              return;
            }

            if (msg?.type === "candidate") {
              if (!msg.candidate?.candidate) return;
              if (remoteDescriptionSet) {
                pc.addIceCandidate(msg.candidate).catch(() => {});
              } else {
                pendingCandidates.push(msg.candidate);
              }
              return;
            }

            if (msg?.type !== "answer") return;
            if (answered) return;
            answered = true;
            clearTimeout(timeout);
            resolve(msg);
          };
          ws.addEventListener("message", onMessage);
        });

        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);

        await new Promise((resolve) => {
          if (pc.iceGatheringState === "complete") return resolve();
          const onState = () => {
            if (pc.iceGatheringState !== "complete") return;
            pc.removeEventListener("icegatheringstatechange", onState);
            resolve();
          };
          pc.addEventListener("icegatheringstatechange", onState);
        });

        if (!pc.localDescription?.sdp) {
          throw new Error("missing local description");
        }

        ws.send(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: pc.localDescription.sdp } }));

        const answerMsg = await answerPromise;
        if (answerMsg?.type !== "answer" || !answerMsg.sdp?.sdp) {
          throw new Error("invalid answer message shape");
        }

        await pc.setRemoteDescription(answerMsg.sdp);
        remoteDescriptionSet = true;
        for (const candidate of pendingCandidates) {
          await pc.addIceCandidate(candidate);
        }

        await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for datachannel open")), 10_000);
          dc.addEventListener(
            "open",
            () => {
              clearTimeout(timeout);
              resolve();
            },
            { once: true },
          );
          dc.addEventListener(
            "error",
            () => {
              clearTimeout(timeout);
              reject(new Error("datachannel error"));
            },
            { once: true },
          );
        });

        // PING per docs/l2-tunnel-protocol.md: magic (0xA2) + ver (0x03) + type (0x01) + flags (0).
        dc.send(new Uint8Array([0xa2, 0x03, 0x01, 0x00]));

        const res = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for PONG")), 10_000);
          dc.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
        });

        ws.close();
        pc.close();
        return Array.from(res);
      },
      { relayPort: relay.port, apiKey },
    );

    expect(pong).toEqual([0xa2, 0x03, 0x02, 0x00]); // PONG

    const debug = await page.request.get(`http://127.0.0.1:${backend.port}/debug`);
    expect(debug.ok()).toBeTruthy();
    const debugJSON = await debug.json();
    expect(debugJSON.origin).toBe(expectedOrigin);
    expect(debugJSON.token).toBe(apiKey);
    expect(debugJSON.tokenSource).toBe("subprotocol");
  } finally {
    await Promise.all([web.close(), relay.kill(), backend.kill()]);
  }
});
