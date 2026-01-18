import assert from "node:assert/strict";
import { randomInt } from "node:crypto";
import { performance } from "node:perf_hooks";

import { unrefBestEffort } from "../../src/unref_safe.js";
import { wsCloseSafe, wsSendSafe } from "../../scripts/_shared/ws_safe.js";
import WebSocket from "../../tools/minimal_ws.js";
import {
  decodeL2Message,
  encodeL2Frame,
  encodePong,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
} from "../../web/src/shared/l2TunnelProtocol.ts";

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
} from "../../prototype/nt-arch-rfc/packets.js";

const ETHERTYPE_ARP = 0x0806;
const ETHERTYPE_IPV4 = 0x0800;

function wrapL2TunnelEthernetFrame(ethernetFrame) {
  // `encodeL2Frame()` returns a `Uint8Array`. Convert to `Buffer` because the probe
  // historically used Buffer payloads throughout (and `tools/minimal_ws.js` uses
  // Buffer internally).
  return Buffer.from(encodeL2Frame(ethernetFrame));
}

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
      unrefBestEffort(timeout);
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

const DHCP_MAGIC_COOKIE = Buffer.from([99, 130, 83, 99]);
const DHCP_CLIENT_PORT = 68;
const DHCP_SERVER_PORT = 67;

const DHCP_MESSAGE_TYPE = {
  DISCOVER: 1,
  OFFER: 2,
  REQUEST: 3,
  ACK: 5,
};

function encodeDhcpDiscover({ xid, clientMac }) {
  const out = Buffer.alloc(240);
  out.writeUInt8(1, 0); // BOOTREQUEST
  out.writeUInt8(1, 1); // ethernet
  out.writeUInt8(6, 2); // hlen
  out.writeUInt32BE(xid >>> 0, 4);
  out.writeUInt16BE(0x8000, 10); // broadcast
  clientMac.copy(out, 28);
  DHCP_MAGIC_COOKIE.copy(out, 236);
  const opts = Buffer.from([
    53, 1, DHCP_MESSAGE_TYPE.DISCOVER,
    55, 3, 1, 3, 6, // subnet mask, router, dns
    255,
  ]);
  return Buffer.concat([out, opts]);
}

function encodeDhcpRequest({ xid, clientMac, requestedIp, serverId }) {
  const out = Buffer.alloc(240);
  out.writeUInt8(1, 0); // BOOTREQUEST
  out.writeUInt8(1, 1); // ethernet
  out.writeUInt8(6, 2); // hlen
  out.writeUInt32BE(xid >>> 0, 4);
  out.writeUInt16BE(0x8000, 10); // broadcast
  clientMac.copy(out, 28);
  DHCP_MAGIC_COOKIE.copy(out, 236);
  const opts = Buffer.concat([
    Buffer.from([53, 1, DHCP_MESSAGE_TYPE.REQUEST]),
    Buffer.from([50, 4]),
    requestedIp,
    Buffer.from([54, 4]),
    serverId,
    Buffer.from([55, 3, 1, 3, 6]),
    Buffer.from([255]),
  ]);
  return Buffer.concat([out, opts]);
}

function parseDhcpMessage(buf) {
  assert.ok(buf.length >= 240, "DHCP message too short");
  assert.ok(buf.subarray(236, 240).equals(DHCP_MAGIC_COOKIE), "DHCP magic cookie missing");
  const xid = buf.readUInt32BE(4);
  const yiaddr = buf.subarray(16, 20);
  const chaddr = buf.subarray(28, 34);

  let messageType = null;
  let serverId = null;
  let router = null;
  let dns = null;

  let off = 240;
  while (off < buf.length) {
    const code = buf.readUInt8(off++);
    if (code === 0) continue;
    if (code === 255) break;
    if (off >= buf.length) break;
    const len = buf.readUInt8(off++);
    if (off + len > buf.length) break;
    const data = buf.subarray(off, off + len);
    off += len;
    if (code === 53 && len === 1) messageType = data.readUInt8(0);
    if (code === 54 && len === 4) serverId = data;
    if (code === 3 && len >= 4) router = data.subarray(0, 4);
    if (code === 6 && len >= 4) dns = data.subarray(0, 4);
  }

  return { xid, yiaddr, chaddr, messageType, serverId, router, dns };
}

async function runL2TunnelProbe({
  url,
  guestMac = "02:00:00:00:00:02",
  dnsName = "echo.local",
  echoPort,
  throughputBytes = 64 * 1024,
} = {}) {
  assert.ok(url, "url is required");
  assert.ok(Number.isInteger(echoPort), "echoPort is required");

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

  function sendEthernetFrame(frameBuf) {
    if (!wsSendSafe(ws, wrapL2TunnelEthernetFrame(frameBuf))) {
      throw new Error("Failed to send L2 ethernet frame");
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

  const guestMacBuf = macToBuffer(guestMac);

  // ---- DHCP handshake (Discover -> Offer -> Request -> Ack) ----
  const xid = randomInt(0, 0xffffffff) >>> 0;
  const dhcpDiscover = encodeDhcpDiscover({ xid, clientMac: guestMacBuf });
  const udpDiscover = encodeUDP({
    srcPort: DHCP_CLIENT_PORT,
    dstPort: DHCP_SERVER_PORT,
    payload: dhcpDiscover,
    srcIp: ipToBuffer("0.0.0.0"),
    dstIp: ipToBuffer("255.255.255.255"),
  });
  const ipDiscover = encodeIPv4({
    srcIp: ipToBuffer("0.0.0.0"),
    dstIp: ipToBuffer("255.255.255.255"),
    protocol: 17,
    payload: udpDiscover,
  });
  sendEthernetFrame(
    encodeEthernetFrame({
      dstMac: Buffer.from("ffffffffffff", "hex"),
      srcMac: guestMacBuf,
      ethertype: ETHERTYPE_IPV4,
      payload: ipDiscover,
    }),
  );

  const offerFrame = await waitFor((frame) => {
    try {
      const eth = parseEthernetFrame(frame);
      if (eth.ethertype !== ETHERTYPE_IPV4) return false;
      const ip = parseIPv4(eth.payload);
      if (ip.protocol !== 17) return false;
      const udp = parseUDP(ip.payload);
      if (udp.srcPort !== DHCP_SERVER_PORT || udp.dstPort !== DHCP_CLIENT_PORT) return false;
      const dhcp = parseDhcpMessage(udp.payload);
      return dhcp.xid === xid && dhcp.messageType === DHCP_MESSAGE_TYPE.OFFER;
    } catch {
      return false;
    }
  }, 5000);

  const offer = (() => {
    const eth = parseEthernetFrame(offerFrame);
    const ip = parseIPv4(eth.payload);
    const udp = parseUDP(ip.payload);
    return parseDhcpMessage(udp.payload);
  })();

  const serverId = offer.serverId ?? ipToBuffer("10.0.2.2");
  const requestedIp = offer.yiaddr;
  const dhcpRequest = encodeDhcpRequest({
    xid,
    clientMac: guestMacBuf,
    requestedIp,
    serverId,
  });
  const udpRequest = encodeUDP({
    srcPort: DHCP_CLIENT_PORT,
    dstPort: DHCP_SERVER_PORT,
    payload: dhcpRequest,
    srcIp: ipToBuffer("0.0.0.0"),
    dstIp: ipToBuffer("255.255.255.255"),
  });
  const ipRequest = encodeIPv4({
    srcIp: ipToBuffer("0.0.0.0"),
    dstIp: ipToBuffer("255.255.255.255"),
    protocol: 17,
    payload: udpRequest,
  });
  sendEthernetFrame(
    encodeEthernetFrame({
      dstMac: Buffer.from("ffffffffffff", "hex"),
      srcMac: guestMacBuf,
      ethertype: ETHERTYPE_IPV4,
      payload: ipRequest,
    }),
  );

  const ackFrame = await waitFor((frame) => {
    try {
      const eth = parseEthernetFrame(frame);
      if (eth.ethertype !== ETHERTYPE_IPV4) return false;
      const ip = parseIPv4(eth.payload);
      if (ip.protocol !== 17) return false;
      const udp = parseUDP(ip.payload);
      if (udp.srcPort !== DHCP_SERVER_PORT || udp.dstPort !== DHCP_CLIENT_PORT) return false;
      const dhcp = parseDhcpMessage(udp.payload);
      return dhcp.xid === xid && dhcp.messageType === DHCP_MESSAGE_TYPE.ACK;
    } catch {
      return false;
    }
  }, 5000);

  const ack = (() => {
    const eth = parseEthernetFrame(ackFrame);
    const ip = parseIPv4(eth.payload);
    const udp = parseUDP(ip.payload);
    return parseDhcpMessage(udp.payload);
  })();

  const guestIp = bufferToIp(ack.yiaddr);
  const gatewayIp = bufferToIp(ack.router ?? serverId);
  const dnsIp = bufferToIp(ack.dns ?? serverId);

  const guestIpBuf = ipToBuffer(guestIp);
  const gatewayIpBuf = ipToBuffer(gatewayIp);
  const dnsIpBuf = ipToBuffer(dnsIp);

  // ---- ARP probe (discover gateway MAC) ----
  const arpRequest = encodeArp({
    opcode: 1,
    senderMac: guestMacBuf,
    senderIp: guestIpBuf,
    targetMac: Buffer.alloc(6),
    targetIp: gatewayIpBuf,
  });
  sendEthernetFrame(
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
    dstIp: dnsIpBuf,
  });
  const ipPayload = encodeIPv4({
    srcIp: guestIpBuf,
    dstIp: dnsIpBuf,
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
  sendEthernetFrame(dnsFrame);
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
  }, 5000);
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
  sendEthernetFrame(
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
  }, 5000);
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
  sendEthernetFrame(
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
    sendEthernetFrame(
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
    sendEthernetFrame(
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
    dhcp: { guestIp, gatewayIp, dnsIp },
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

export { runL2TunnelProbe };
