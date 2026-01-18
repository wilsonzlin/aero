import assert from "node:assert/strict";
import { randomInt } from "node:crypto";
import { performance } from "node:perf_hooks";

import WebSocket from "../../tools/minimal_ws.js";
import { wsCloseSafe, wsSendSafe } from "../../scripts/_shared/ws_safe.js";

import {
  TCP_FLAGS,
  bufferToIp,
  encodeArp,
  encodeDnsQuery,
  encodeEthernetFrame,
  encodeIPv4,
  encodeTCP,
  encodeUDP,
  ipToBuffer,
  macToBuffer,
  parseArp,
  parseDnsResponseA,
  parseEthernetFrame,
  parseIPv4,
  parseTCP,
  parseUDP,
} from "./packets.js";
import {
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  decodeL2Message,
  encodeL2Frame,
  encodePong,
} from "./l2_tunnel_proto.js";

const ETHERTYPE_ARP = 0x0806;
const ETHERTYPE_IPV4 = 0x0800;

class FrameQueue {
  constructor() {
    this.queue = [];
    this.waiters = [];
  }

  push(frame) {
    if (this.waiters.length > 0) {
      const waiter = this.waiters.shift();
      waiter.resolve(frame);
      return;
    }
    this.queue.push(frame);
  }

  async shift(timeoutMs) {
    if (this.queue.length > 0) return this.queue.shift();
    return await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        const idx = this.waiters.indexOf(waiter);
        if (idx !== -1) this.waiters.splice(idx, 1);
        reject(new Error(`Timed out waiting for frame after ${timeoutMs}ms`));
      }, timeoutMs);
      const waiter = {
        resolve: (frame) => {
          clearTimeout(timeout);
          resolve(frame);
        },
      };
      this.waiters.push(waiter);
    });
  }
}

async function connectWebSocket(url) {
  const ws = new WebSocket(url, L2_TUNNEL_SUBPROTOCOL);
  ws.binaryType = "nodebuffer";
  await new Promise((resolve, reject) => {
    ws.on("open", resolve);
    ws.on("error", reject);
  });
  return ws;
}

async function runNetworkingProbe({
  url,
  guestMac = "02:00:00:00:00:01",
  guestIp = "10.0.2.15",
  gatewayIp = "10.0.2.2",
  dnsName = "echo.local",
  echoPort,
  throughputBytes = 256 * 1024,
} = {}) {
  assert.ok(url, "url is required");
  assert.ok(Number.isInteger(echoPort), "echoPort is required");

  const guestMacBuf = macToBuffer(guestMac);
  const guestIpBuf = ipToBuffer(guestIp);
  const gatewayIpBuf = ipToBuffer(gatewayIp);

  const ws = await connectWebSocket(url);
  const frames = new FrameQueue();

  ws.on("message", (msg) => {
    const buf = Buffer.isBuffer(msg) ? msg : Buffer.from(msg);
    let decoded;
    try {
      decoded = decodeL2Message(buf);
    } catch {
      return;
    }

    if (decoded.type === L2_TUNNEL_TYPE_FRAME) {
      frames.push(Buffer.from(decoded.payload));
      return;
    }

    if (decoded.type === L2_TUNNEL_TYPE_PING) {
      wsSendSafe(ws, encodePong(decoded.payload));
    }
  });

  function sendFrame(frameBuf) {
    if (!wsSendSafe(ws, encodeL2Frame(frameBuf))) {
      throw new Error("Failed to send L2 frame");
    }
  }

  async function waitFor(predicate, timeoutMs = 2000) {
    const deadline = performance.now() + timeoutMs;
    while (true) {
      const remaining = Math.max(0, deadline - performance.now());
      const frame = await frames.shift(Math.max(1, remaining));
      if (predicate(frame)) return frame;
    }
  }

  // ---- ARP probe (discover gateway MAC) ----
  const arpRequest = encodeArp({
    opcode: 1,
    senderMac: guestMacBuf,
    senderIp: guestIpBuf,
    targetMac: Buffer.alloc(6),
    targetIp: gatewayIpBuf,
  });
  sendFrame(
    encodeEthernetFrame({
      dstMac: Buffer.from("ffffffffffff", "hex"),
      srcMac: guestMacBuf,
      ethertype: ETHERTYPE_ARP,
      payload: arpRequest,
    }),
  );

  const arpReplyFrame = await waitFor((frame) => {
    try {
      const eth = parseEthernetFrame(frame);
      if (eth.ethertype !== ETHERTYPE_ARP) return false;
      const arp = parseArp(eth.payload);
      return arp.opcode === 2 && arp.senderIp.equals(gatewayIpBuf);
    } catch {
      return false;
    }
  });
  const arpReplyEth = parseEthernetFrame(arpReplyFrame);
  const arpReply = parseArp(arpReplyEth.payload);
  const gatewayMacBuf = Buffer.from(arpReply.senderMac);

  // ---- DNS probe ----
  const dnsId = randomInt(0, 0xffff);
  const dnsPayload = encodeDnsQuery({ id: dnsId, name: dnsName });
  const dnsSrcPort = randomInt(49152, 65535);
  const udpPayload = encodeUDP({
    srcPort: dnsSrcPort,
    dstPort: 53,
    payload: dnsPayload,
    srcIp: guestIpBuf,
    dstIp: gatewayIpBuf,
  });
  const ipPayload = encodeIPv4({
    srcIp: guestIpBuf,
    dstIp: gatewayIpBuf,
    protocol: 17,
    payload: udpPayload,
  });
  const dnsFrame = encodeEthernetFrame({
    dstMac: gatewayMacBuf,
    srcMac: guestMacBuf,
    ethertype: ETHERTYPE_IPV4,
    payload: ipPayload,
  });

  const dnsStart = performance.now();
  sendFrame(dnsFrame);
  const dnsReplyFrame = await waitFor((frame) => {
    try {
      const eth = parseEthernetFrame(frame);
      if (eth.ethertype !== ETHERTYPE_IPV4) return false;
      const ip = parseIPv4(eth.payload);
      if (ip.protocol !== 17) return false;
      const udp = parseUDP(ip.payload);
      if (udp.dstPort !== dnsSrcPort) return false;
      const dns = parseDnsResponseA(udp.payload);
      return dns.id === dnsId && dns.name === dnsName;
    } catch {
      return false;
    }
  });
  const dnsRttMs = performance.now() - dnsStart;
  const dnsReply = (() => {
    const eth = parseEthernetFrame(dnsReplyFrame);
    const ip = parseIPv4(eth.payload);
    const udp = parseUDP(ip.payload);
    return parseDnsResponseA(udp.payload);
  })();

  const resolvedIp = dnsReply.ip;
  const resolvedIpBuf = ipToBuffer(resolvedIp);

  // ---- TCP echo probe ----
  const clientPort = randomInt(49152, 65535);
  const clientIsn = randomInt(0, 0xffffffff) >>> 0;
  const synPayload = encodeTCP({
    srcPort: clientPort,
    dstPort: echoPort,
    seq: clientIsn,
    ack: 0,
    flags: TCP_FLAGS.SYN,
    srcIp: guestIpBuf,
    dstIp: resolvedIpBuf,
  });
  sendFrame(
    encodeEthernetFrame({
      dstMac: gatewayMacBuf,
      srcMac: guestMacBuf,
      ethertype: ETHERTYPE_IPV4,
      payload: encodeIPv4({
        srcIp: guestIpBuf,
        dstIp: resolvedIpBuf,
        protocol: 6,
        payload: synPayload,
      }),
    }),
  );

  const tcpSynStart = performance.now();
  const synAckFrame = await waitFor((frame) => {
    try {
      const eth = parseEthernetFrame(frame);
      if (eth.ethertype !== ETHERTYPE_IPV4) return false;
      const ip = parseIPv4(eth.payload);
      if (ip.protocol !== 6) return false;
      if (!ip.srcIp.equals(resolvedIpBuf)) return false;
      const tcp = parseTCP(ip.payload);
      if (tcp.dstPort !== clientPort) return false;
      const want = TCP_FLAGS.SYN | TCP_FLAGS.ACK;
      return (
        tcp.srcPort === echoPort &&
        (tcp.flags & want) === want &&
        tcp.ack === (clientIsn + 1) >>> 0
      );
    } catch {
      return false;
    }
  });
  const tcpConnectRttMs = performance.now() - tcpSynStart;

  const synAckTcp = (() => {
    const eth = parseEthernetFrame(synAckFrame);
    const ip = parseIPv4(eth.payload);
    return parseTCP(ip.payload);
  })();

  const serverIsn = synAckTcp.seq >>> 0;
  let nextClientSeq = (clientIsn + 1) >>> 0;
  let nextServerSeq = (serverIsn + 1) >>> 0;

  // ACK the SYN-ACK.
  const ackPayload = encodeTCP({
    srcPort: clientPort,
    dstPort: echoPort,
    seq: nextClientSeq,
    ack: nextServerSeq,
    flags: TCP_FLAGS.ACK,
    srcIp: guestIpBuf,
    dstIp: resolvedIpBuf,
  });
  sendFrame(
    encodeEthernetFrame({
      dstMac: gatewayMacBuf,
      srcMac: guestMacBuf,
      ethertype: ETHERTYPE_IPV4,
      payload: encodeIPv4({
        srcIp: guestIpBuf,
        dstIp: resolvedIpBuf,
        protocol: 6,
        payload: ackPayload,
      }),
    }),
  );

  const payload = Buffer.allocUnsafe(throughputBytes);
  for (let i = 0; i < payload.length; i++) payload[i] = i & 0xff;

  const maxChunk = 1200;
  const txStart = performance.now();
  for (let off = 0; off < payload.length; off += maxChunk) {
    const chunk = payload.subarray(off, off + maxChunk);
    const dataSeg = encodeTCP({
      srcPort: clientPort,
      dstPort: echoPort,
      seq: nextClientSeq,
      ack: nextServerSeq,
      flags: TCP_FLAGS.PSH | TCP_FLAGS.ACK,
      payload: chunk,
      srcIp: guestIpBuf,
      dstIp: resolvedIpBuf,
    });
    sendFrame(
      encodeEthernetFrame({
        dstMac: gatewayMacBuf,
        srcMac: guestMacBuf,
        ethertype: ETHERTYPE_IPV4,
        payload: encodeIPv4({
          srcIp: guestIpBuf,
          dstIp: resolvedIpBuf,
          protocol: 6,
          payload: dataSeg,
        }),
      }),
    );
    nextClientSeq = (nextClientSeq + chunk.length) >>> 0;
  }

  const received = [];
  let receivedLen = 0;

  while (receivedLen < payload.length) {
    const frame = await waitFor((f) => {
      try {
        const eth = parseEthernetFrame(f);
        if (eth.ethertype !== ETHERTYPE_IPV4) return false;
        const ip = parseIPv4(eth.payload);
        if (ip.protocol !== 6) return false;
        if (!ip.srcIp.equals(resolvedIpBuf)) return false;
        const tcp = parseTCP(ip.payload);
        if (tcp.dstPort !== clientPort) return false;
        if (tcp.srcPort !== echoPort) return false;
        if ((tcp.flags & TCP_FLAGS.ACK) === 0) return false;
        if (tcp.payload.length === 0) return false;
        return tcp.seq === nextServerSeq;
      } catch {
        return false;
      }
    }, 5000);

    const eth = parseEthernetFrame(frame);
    const ip = parseIPv4(eth.payload);
    const tcp = parseTCP(ip.payload);
    received.push(tcp.payload);
    receivedLen += tcp.payload.length;
    nextServerSeq = (nextServerSeq + tcp.payload.length) >>> 0;

    // ACK the received data.
    const ack = encodeTCP({
      srcPort: clientPort,
      dstPort: echoPort,
      seq: nextClientSeq,
      ack: nextServerSeq,
      flags: TCP_FLAGS.ACK,
      srcIp: guestIpBuf,
      dstIp: resolvedIpBuf,
    });
    sendFrame(
      encodeEthernetFrame({
        dstMac: gatewayMacBuf,
        srcMac: guestMacBuf,
        ethertype: ETHERTYPE_IPV4,
        payload: encodeIPv4({
          srcIp: guestIpBuf,
          dstIp: resolvedIpBuf,
          protocol: 6,
          payload: ack,
        }),
      }),
    );
  }

  const txDurationMs = performance.now() - txStart;
  const echoPayload = Buffer.concat(received, receivedLen).subarray(0, payload.length);
  const throughputMbps = (payload.length * 8) / (txDurationMs / 1000) / 1_000_000;

  wsCloseSafe(ws);

  return {
    arp: { gatewayMac: arpReply.senderMac.toString("hex") },
    dns: { name: dnsName, ip: resolvedIp, rttMs: dnsRttMs },
    tcp: {
      remoteIp: resolvedIp,
      remotePort: echoPort,
      connectRttMs: tcpConnectRttMs,
      throughputBytes: payload.length,
      throughputMbps,
      ok: echoPayload.equals(payload),
    },
  };
}

export { runNetworkingProbe };
