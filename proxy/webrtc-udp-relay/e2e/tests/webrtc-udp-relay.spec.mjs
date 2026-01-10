import { test, expect } from "@playwright/test";
import dgram from "node:dgram";
import http from "node:http";
import { spawn } from "node:child_process";
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

async function spawnRelayServer() {
  const relayPath = path.join(__dirname, "..", "relay-server", "server.mjs");

  const child = spawn(process.execPath, [relayPath], {
    env: {
      ...process.env,
      AUTH_MODE: "none",
      BIND_HOST: "127.0.0.1",
      PORT: "0",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stderr.on("data", (chunk) => {
    // Surface relay crashes in the test output.
    process.stderr.write(chunk);
  });

  const port = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("relay did not start")), 10_000);
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
      reject(new Error(`relay exited early (${code ?? "unknown"})`));
    });
  });

  return {
    port,
    kill: async () => {
      if (child.exitCode !== null) return;
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("exit", resolve));
    },
  };
}

test("relays a UDP datagram via a Chromium WebRTC DataChannel", async ({ page }) => {
  const echo = await startUdpEchoServer("udp4", "127.0.0.1");
  const relay = await spawnRelayServer();
  const web = await startWebServer();

  try {
    await page.goto(web.url);

    const echoed = await page.evaluate(
      async ({ relayPort, echoPort }) => {
        const iceServers = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
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

        ws.send(JSON.stringify({ version: 1, offer: pc.localDescription }));

        const answerMsg = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          ws.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(JSON.parse(event.data));
            },
            { once: true },
          );
        });

        if (answerMsg?.version !== 1 || !answerMsg.answer?.sdp) {
          throw new Error("invalid answer message");
        }

        await pc.setRemoteDescription(answerMsg.answer);

        await new Promise((resolve, reject) => {
          dc.addEventListener("open", () => resolve(), { once: true });
          dc.addEventListener("error", () => reject(new Error("datachannel error")), { once: true });
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
        const iceServers = await fetch(`http://127.0.0.1:${relayPort}/webrtc/ice`).then((r) => r.json());

        const ws = new WebSocket(`ws://127.0.0.1:${relayPort}/webrtc/signal`);
        await new Promise((resolve, reject) => {
          ws.addEventListener("open", () => resolve(), { once: true });
          ws.addEventListener("error", () => reject(new Error("ws error")), { once: true });
        });

        const pc = new RTCPeerConnection({ iceServers });
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

        ws.send(JSON.stringify({ version: 1, offer: pc.localDescription }));

        const answerMsg = await new Promise((resolve, reject) => {
          const timeout = setTimeout(() => reject(new Error("timed out waiting for answer")), 10_000);
          ws.addEventListener(
            "message",
            (event) => {
              clearTimeout(timeout);
              resolve(JSON.parse(event.data));
            },
            { once: true },
          );
        });

        if (answerMsg?.version !== 1 || !answerMsg.answer?.sdp) {
          throw new Error("invalid answer message");
        }

        await pc.setRemoteDescription(answerMsg.answer);

        await new Promise((resolve, reject) => {
          dc.addEventListener("open", () => resolve(), { once: true });
          dc.addEventListener("error", () => reject(new Error("datachannel error")), { once: true });
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
