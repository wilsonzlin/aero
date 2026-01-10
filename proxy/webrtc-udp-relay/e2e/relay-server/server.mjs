import http from "node:http";
import process from "node:process";
import { WebSocketServer } from "ws";
import { RTCPeerConnection } from "werift";
import dgram from "node:dgram";

const bindHost = process.env.BIND_HOST || "127.0.0.1";
const port = Number.parseInt(process.env.PORT ?? "0", 10);

if (process.env.AUTH_MODE && process.env.AUTH_MODE !== "none") {
  // Keep secrets out of test runs. This relay implementation only supports `AUTH_MODE=none`.
  console.error(`unsupported AUTH_MODE=${process.env.AUTH_MODE}`);
  process.exit(1);
}

const server = http.createServer((req, res) => {
  // Minimal API surface required by the browser test harness.
  if (req.method === "GET" && req.url === "/webrtc/ice") {
    // Localhost E2E: host candidates are sufficient, no STUN/TURN required.
    res.statusCode = 200;
    res.setHeader("content-type", "application/json");
    res.setHeader("access-control-allow-origin", "*");
    res.end("[]");
    return;
  }

  res.statusCode = 404;
  res.end("not found");
});

const wss = new WebSocketServer({ noServer: true });

server.on("upgrade", (req, socket, head) => {
  if (req.url !== "/webrtc/signal") {
    socket.destroy();
    return;
  }

  wss.handleUpgrade(req, socket, head, (ws) => {
    wss.emit("connection", ws, req);
  });
});

wss.on("connection", async (ws) => {
  const pc = new RTCPeerConnection({
    // Avoid external network dependencies; host candidates are enough for loopback.
    iceServers: [],
  });

  const udpSocket = dgram.createSocket("udp4");
  await new Promise((resolve, reject) => {
    udpSocket.once("error", reject);
    udpSocket.bind(0, bindHost, () => {
      udpSocket.off("error", reject);
      resolve();
    });
  });

  const sendSignal = (msg) => {
    if (ws.readyState === ws.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  };

  pc.onDataChannel.subscribe((channel) => {
    // The browser creates a single unordered/unreliable channel named "udp".
    channel.onMessage.subscribe(async (data) => {
      const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
      if (buf.length < 8) return;

      // v1 frame: guest_port (u16) + remote_ipv4 (4B) + remote_port (u16) + payload (N).
      // See: ../../PROTOCOL.md (kept in sync with Go udpproto implementation).
      const guestPort = buf.readUInt16BE(0);
      const ip = `${buf[2]}.${buf[3]}.${buf[4]}.${buf[5]}`;
      const destPort = buf.readUInt16BE(6);
      const payload = buf.subarray(8);

      udpSocket.send(payload, destPort, ip);

      const response = await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("udp timeout")), 5_000);
        udpSocket.once("message", (msg, rinfo) => {
          clearTimeout(timeout);
          resolve({ msg, rinfo });
        });
      });

      const { msg, rinfo } = response;
      const src = rinfo.address.split(".").map((x) => Number.parseInt(x, 10));
      const frame = Buffer.alloc(8 + msg.length);
      frame.writeUInt16BE(guestPort, 0);
      frame[2] = src[0] ?? 0;
      frame[3] = src[1] ?? 0;
      frame[4] = src[2] ?? 0;
      frame[5] = src[3] ?? 0;
      frame.writeUInt16BE(rinfo.port, 6);
      msg.copy(frame, 8);
      channel.send(frame);
    });
  });

  ws.on("message", async (raw) => {
    const msg = JSON.parse(raw.toString());
    if (msg?.version !== 1 || !msg.offer?.sdp) return;

    await pc.setRemoteDescription(msg.offer);
    await pc.setLocalDescription(await pc.createAnswer());

    if (pc.iceGatheringState !== "complete") {
      await pc.iceGatheringStateChange.watch((state) => state === "complete", 10_000);
    }

    sendSignal({ version: 1, answer: pc.localDescription });
  });

  const close = () => {
    try {
      udpSocket.close();
    } catch {
      // ignore
    }
    try {
      pc.close();
    } catch {
      // ignore
    }
  };

  ws.on("close", close);
  ws.on("error", close);
});

await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(port, bindHost, () => {
    server.off("error", reject);
    resolve();
  });
});

const actualPort = server.address().port;
process.stdout.write(`READY ${actualPort}\n`);
