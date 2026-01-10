import http from "node:http";
import net from "node:net";
import process from "node:process";
import { WebSocketServer } from "ws";
import { RTCPeerConnection } from "werift";
import dgram from "node:dgram";

const bindHost = process.env.BIND_HOST || "127.0.0.1";
const port = Number.parseInt(process.env.PORT ?? "0", 10);

const V2_MAGIC = 0xa2;
const V2_VERSION = 0x02;
const AF_IPV4 = 0x04;
const AF_IPV6 = 0x06;

function ipv4StringToBytes(ip) {
  const parts = ip.split(".");
  if (parts.length !== 4) {
    throw new Error(`invalid ipv4 address: ${ip}`);
  }
  return parts.map((p) => Number.parseInt(p, 10));
}

function ipv6BytesToString(bytes) {
  if (bytes.length !== 16) {
    throw new Error("invalid ipv6 byte length");
  }
  const groups = [];
  for (let i = 0; i < 16; i += 2) {
    groups.push(((bytes[i] << 8) | bytes[i + 1]).toString(16));
  }
  // No compression required; any valid textual form is acceptable for dgram.
  return groups.join(":");
}

function ipv6StringToBytes(ip) {
  ip = ip.split("%")[0] ?? ip;

  // Expand IPv4-embedded suffix (e.g. ::ffff:192.0.2.1) into two hextets.
  if (ip.includes(".")) {
    const idx = ip.lastIndexOf(":");
    if (idx === -1) throw new Error(`invalid ipv6 address: ${ip}`);
    const v4 = ipv4StringToBytes(ip.slice(idx + 1));
    const hi = (v4[0] << 8) | v4[1];
    const lo = (v4[2] << 8) | v4[3];
    ip = `${ip.slice(0, idx)}:${hi.toString(16)}:${lo.toString(16)}`;
  }

  const parts = ip.split("::");
  if (parts.length > 2) throw new Error(`invalid ipv6 address: ${ip}`);
  const left = parts[0] ? parts[0].split(":").filter(Boolean) : [];
  const right = parts.length === 2 && parts[1] ? parts[1].split(":").filter(Boolean) : [];
  const missing = 8 - (left.length + right.length);
  if (missing < 0) throw new Error(`invalid ipv6 address: ${ip}`);
  const groups = [...left, ...Array(missing).fill("0"), ...right];
  if (groups.length !== 8) throw new Error(`invalid ipv6 address: ${ip}`);

  const buf = Buffer.alloc(16);
  groups.forEach((g, i) => {
    const n = Number.parseInt(g, 16);
    if (!Number.isFinite(n) || n < 0 || n > 0xffff) throw new Error(`invalid ipv6 group: ${g}`);
    buf.writeUInt16BE(n, i * 2);
  });
  return buf;
}

function decodeFrame(buf) {
  if (buf.length >= 2 && buf[0] === V2_MAGIC && buf[1] === V2_VERSION) {
    if (buf.length < 12) return null;
    const af = buf[2];
    if (buf[3] !== 0) return null;
    const guestPort = buf.readUInt16BE(4);
    let offset = 6;

    let ip;
    if (af === AF_IPV4) {
      if (buf.length < offset + 4 + 2) return null;
      ip = `${buf[offset]}.${buf[offset + 1]}.${buf[offset + 2]}.${buf[offset + 3]}`;
      offset += 4;
    } else if (af === AF_IPV6) {
      if (buf.length < offset + 16 + 2) return null;
      ip = ipv6BytesToString(buf.subarray(offset, offset + 16));
      offset += 16;
    } else {
      return null;
    }

    const remotePort = buf.readUInt16BE(offset);
    offset += 2;
    const payload = buf.subarray(offset);

    return { version: 2, af, guestPort, ip, remotePort, payload };
  }

  // v1 frame: guest_port (u16) + remote_ipv4 (4B) + remote_port (u16) + payload (N).
  if (buf.length < 8) return null;
  const guestPort = buf.readUInt16BE(0);
  const ip = `${buf[2]}.${buf[3]}.${buf[4]}.${buf[5]}`;
  const remotePort = buf.readUInt16BE(6);
  const payload = buf.subarray(8);
  return { version: 1, af: AF_IPV4, guestPort, ip, remotePort, payload };
}

function encodeV1({ guestPort, remoteIP, remotePort, payload }) {
  const src = ipv4StringToBytes(remoteIP);
  const frame = Buffer.alloc(8 + payload.length);
  frame.writeUInt16BE(guestPort, 0);
  frame[2] = src[0] ?? 0;
  frame[3] = src[1] ?? 0;
  frame[4] = src[2] ?? 0;
  frame[5] = src[3] ?? 0;
  frame.writeUInt16BE(remotePort, 6);
  Buffer.from(payload).copy(frame, 8);
  return frame;
}

function encodeV2({ guestPort, remoteIP, remotePort, payload }) {
  let af;
  let ipBytes;
  if (net.isIPv4(remoteIP)) {
    af = AF_IPV4;
    ipBytes = Buffer.from(ipv4StringToBytes(remoteIP));
  } else if (net.isIPv6(remoteIP)) {
    af = AF_IPV6;
    ipBytes = ipv6StringToBytes(remoteIP);
  } else {
    throw new Error(`invalid ip for v2 frame: ${remoteIP}`);
  }

  const headerLen = 4 + 2 + ipBytes.length + 2;
  const frame = Buffer.alloc(headerLen + payload.length);
  frame[0] = V2_MAGIC;
  frame[1] = V2_VERSION;
  frame[2] = af;
  frame[3] = 0;
  frame.writeUInt16BE(guestPort, 4);
  ipBytes.copy(frame, 6);
  frame.writeUInt16BE(remotePort, 6 + ipBytes.length);
  Buffer.from(payload).copy(frame, headerLen);
  return frame;
}

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

  const udpSocket4 = dgram.createSocket("udp4");
  await new Promise((resolve, reject) => {
    udpSocket4.once("error", reject);
    udpSocket4.bind(0, bindHost, () => {
      udpSocket4.off("error", reject);
      resolve();
    });
  });

  let udpSocket6 = null;
  try {
    udpSocket6 = dgram.createSocket("udp6");
    await new Promise((resolve, reject) => {
      udpSocket6.once("error", reject);
      udpSocket6.bind(0, "::1", () => {
        udpSocket6.off("error", reject);
        resolve();
      });
    });
  } catch (err) {
    try {
      udpSocket6?.close();
    } catch {
      // ignore
    }
    udpSocket6 = null;
  }

  const sendSignal = (msg) => {
    if (ws.readyState === ws.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  };

  pc.onDataChannel.subscribe((channel) => {
    // The browser creates a single unordered/unreliable channel named "udp".
    channel.onMessage.subscribe(async (data) => {
      const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
      const frame = decodeFrame(buf);
      if (!frame) return;

      const socket = frame.af === AF_IPV6 ? udpSocket6 : udpSocket4;
      if (!socket) return;

      socket.send(frame.payload, frame.remotePort, frame.ip);

      const response = await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("udp timeout")), 5_000);
        socket.once("message", (msg, rinfo) => {
          clearTimeout(timeout);
          resolve({ msg, rinfo });
        });
      });

      const { msg, rinfo } = response;
      const outFrame =
        frame.version === 2
          ? encodeV2({
              guestPort: frame.guestPort,
              remoteIP: rinfo.address,
              remotePort: rinfo.port,
              payload: msg,
            })
          : encodeV1({
              guestPort: frame.guestPort,
              remoteIP: rinfo.address,
              remotePort: rinfo.port,
              payload: msg,
            });

      channel.send(outFrame);
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
      udpSocket4.close();
      udpSocket6?.close();
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
