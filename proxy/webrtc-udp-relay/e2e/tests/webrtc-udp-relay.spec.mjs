import { test, expect } from "@playwright/test";
import dgram from "node:dgram";
import fs from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import { spawn, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

async function startUdpEchoServer(socketType, host) {
  const socket = dgram.createSocket(socketType);
  socket.on("message", (msg, rinfo) => {
    socket.send(msg, rinfo.port, rinfo.address);
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

async function spawnGoReadyServer({ name, pkg, env }) {
  const moduleDir = path.join(__dirname, "..", "..");
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "aero-webrtc-udp-relay-e2e-"));
  const binPath = path.join(tmpDir, name);

  const build = spawnSync("go", ["build", "-o", binPath, pkg], {
    cwd: moduleDir,
    stdio: "inherit",
  });
  if (build.status !== 0) {
    await fs.rm(tmpDir, { recursive: true, force: true });
    throw new Error(`failed to build Go server ${pkg} (exit ${build.status ?? "unknown"})`);
  }

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

    child.on("exit", (code) => {
      clearTimeout(timeout);
      fs.rm(tmpDir, { recursive: true, force: true }).catch(() => {});
      reject(new Error(`${name} exited early (${code ?? "unknown"})`));
    });
  });

  return {
    port,
    kill: async () => {
      if (child.exitCode === null) {
        child.kill("SIGTERM");
        await new Promise((resolve) => child.once("exit", resolve));
      }
      await fs.rm(tmpDir, { recursive: true, force: true });
    },
  };
}

async function spawnL2BackendServer() {
  return spawnGoReadyServer({
    name: "l2-backend-go",
    pkg: "./e2e/l2-backend-go",
    env: {
      BIND_HOST: "127.0.0.1",
      PORT: "0",
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

    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

let relayBinPromise;

async function getRelayBinaryPath() {
  if (!relayBinPromise) {
    relayBinPromise = (async () => {
      const moduleDir = path.join(__dirname, "..", "..");
      const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "aero-webrtc-udp-relay-e2e-"));
      const binName = process.platform === "win32" ? "aero-webrtc-udp-relay.exe" : "aero-webrtc-udp-relay";
      const binPath = path.join(tmpDir, binName);

      const build = spawnSync("go", ["build", "-o", binPath, "./cmd/aero-webrtc-udp-relay"], {
        cwd: moduleDir,
        stdio: "inherit",
      });
      if (build.status !== 0) {
        throw new Error(`failed to build aero-webrtc-udp-relay (exit ${build.status ?? "unknown"})`);
      }

      return binPath;
    })();
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
        // Let the Playwright-served page (random localhost port) talk to the relay.
        ALLOWED_ORIGINS: "*",
        // Keep /webrtc/ice stable even when no STUN/TURN is configured.
        AERO_ICE_SERVERS_JSON: "[]",
        // Allow the UDP echo server on localhost.
        DESTINATION_POLICY_PRESET: "dev",
        ALLOW_PRIVATE_NETWORKS: "true",
        // Ensure IPv4 echo responses can be v2 once the client demonstrates v2 support.
        PREFER_V2: "true",
        // Auth is irrelevant for these tests, so disable it.
        AUTH_MODE: "none",
        // Reduce noise in Playwright output.
        AERO_WEBRTC_UDP_RELAY_LOG_LEVEL: "error",
        ...extraEnv,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    // Surface relay crashes in the test output.
    child.stderr.on("data", (chunk) => process.stderr.write(chunk));

    try {
      await waitForRelayReady(port, child, 20_000);

      return {
        port,
        kill: async () => {
          if (child.exitCode === null) {
            child.kill("SIGTERM");
            await new Promise((resolve) => child.once("exit", resolve));
          }
        },
      };
    } catch (err) {
      lastErr = err;
      if (child.exitCode === null) {
        child.kill("SIGTERM");
        await new Promise((resolve) => child.once("exit", resolve));
      }
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
        const frame = new Uint8Array(24 + payload.length);
        frame[0] = 0xa2;
        frame[1] = 0x02;
        frame[2] = 0x06;
        frame[3] = 0x00;
        frame[4] = (guestPort >> 8) & 0xff;
        frame[5] = guestPort & 0xff;
        // ::1
        frame[21] = 1;
        frame[22] = (echoPort >> 8) & 0xff;
        frame[23] = echoPort & 0xff;
        frame.set(payload, 24);
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

        if (echoedFrame.length < 24) throw new Error("echoed frame too short");
        if (echoedFrame[0] !== 0xa2 || echoedFrame[1] !== 0x02 || echoedFrame[2] !== 0x06 || echoedFrame[3] !== 0x00) {
          throw new Error("v2 header mismatch");
        }

        const echoedGuestPort = (echoedFrame[4] << 8) | echoedFrame[5];
        if (echoedGuestPort !== guestPort) throw new Error("guest port mismatch");
        for (let i = 6; i < 21; i++) {
          if (echoedFrame[i] !== 0) throw new Error("remote ip mismatch");
        }
        if (echoedFrame[21] !== 1) throw new Error("remote ip mismatch");

        const echoedRemotePort = (echoedFrame[22] << 8) | echoedFrame[23];
        if (echoedRemotePort !== echoPort) throw new Error("remote port mismatch");

        const echoedPayload = echoedFrame.slice(24);
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
            const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
            ws.addEventListener(
              "message",
              (event) => {
                clearTimeout(timeout);
                resolve(new Uint8Array(event.data));
              },
              { once: true },
            );
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

        const frame = new Uint8Array(24 + payload.length);
        frame[0] = 0xa2;
        frame[1] = 0x02;
        frame[2] = 0x06;
        frame[3] = 0x00;
        frame[4] = (guestPort >> 8) & 0xff;
        frame[5] = guestPort & 0xff;
        // ::1
        frame[21] = 1;
        frame[22] = (echoPort >> 8) & 0xff;
        frame[23] = echoPort & 0xff;
        frame.set(payload, 24);

        const echoedFrame = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for echoed datagram")), 10_000);
          ws.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(new Uint8Array(event.data));
            },
            { once: true },
          );
          ws.send(frame);
        });

        if (echoedFrame.length < 24) throw new Error("echoed frame too short");
        if (echoedFrame[0] !== 0xa2 || echoedFrame[1] !== 0x02 || echoedFrame[2] !== 0x06 || echoedFrame[3] !== 0x00) {
          throw new Error("v2 header mismatch");
        }

        const echoedPayload = echoedFrame.slice(24);
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

test("bridges an L2 tunnel DataChannel to a backend WebSocket", async ({ page }) => {
  const backend = await spawnL2BackendServer();
  const relay = await spawnRelayServer({
    L2_BACKEND_WS_URL: `ws://127.0.0.1:${backend.port}/l2`,
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
        // L2 tunnel MUST be reliable (no partial reliability). Do not set maxRetransmits/maxPacketLifeTime.
        const dc = pc.createDataChannel("l2", { ordered: false });
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
  } finally {
    await Promise.all([web.close(), relay.kill(), backend.kill()]);
  }
});
