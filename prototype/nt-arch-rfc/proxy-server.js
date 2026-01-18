import net from "node:net";
import { randomInt } from "node:crypto";
import { once } from "node:events";

import { socketWritableLengthExceedsCap } from "../../src/socket_writable_length.js";

import { WebSocketServer } from "../../tools/minimal_ws.js";
import { wsSendSafe } from "../../scripts/_shared/ws_safe.js";

import {
  TCP_FLAGS,
  bufferToIp,
  encodeArp,
  encodeDnsResponseA,
  encodeEthernetFrame,
  encodeIPv4,
  encodeTCP,
  encodeUDP,
  ipToBuffer,
  macToBuffer,
  parseArp,
  parseDnsQuery,
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

function normalizeTcpForwardMap(tcpForward) {
  if (!tcpForward) return new Map();
  if (tcpForward instanceof Map) return tcpForward;
  return new Map(
    Object.entries(tcpForward).map(([k, v]) => [Number.parseInt(k, 10), v]),
  );
}

async function startProxyServer({
  host = "127.0.0.1",
  port = 0,
  gatewayMac = "02:00:00:00:00:02",
  gatewayIp = "10.0.2.2",
  // Return a TEST-NET address by default; the proxy still uses local forwarding.
  dnsA = { "echo.local": "203.0.113.10" },
  tcpForward = {},
  // Defensive: prevent unbounded buffering if the forwarded TCP peer stops reading.
  maxTcpBufferedBytesPerConn = 10 * 1024 * 1024,
  // Test/embedding hook.
  createTcpConnection = net.createConnection,
} = {}) {
  const gatewayMacBuf = macToBuffer(gatewayMac);
  const gatewayIpBuf = ipToBuffer(gatewayIp);
  const tcpForwardMap = normalizeTcpForwardMap(tcpForward);

  const wss = new WebSocketServer({
    host,
    port,
    handleProtocols: (protocols) => {
      // Match the normative protocol requirement from docs/l2-tunnel-protocol.md.
      if (protocols.has(L2_TUNNEL_SUBPROTOCOL)) return L2_TUNNEL_SUBPROTOCOL;
      return false;
    },
  });
  await once(wss, "listening");

  const address = wss.address();
  const actualPort = typeof address === "string" ? port : address.port;
  const url = `ws://${host}:${actualPort}`;

  wss.on("connection", (ws) => {
    ws.binaryType = "nodebuffer";
    const tcpConns = new Map(); // key: `${clientIp}:${clientPort}->${dstIp}:${dstPort}`

    function sendEthernet({ dstMac, srcMac, ethertype, payload }) {
      wsSendSafe(ws, encodeL2Frame(encodeEthernetFrame({ dstMac, srcMac, ethertype, payload })));
    }

    function sendArpReply({ requestEth, requestArp }) {
      const payload = encodeArp({
        opcode: 2,
        senderMac: gatewayMacBuf,
        senderIp: gatewayIpBuf,
        targetMac: requestArp.senderMac,
        targetIp: requestArp.senderIp,
      });
      sendEthernet({
        dstMac: requestEth.srcMac,
        srcMac: gatewayMacBuf,
        ethertype: ETHERTYPE_ARP,
        payload,
      });
    }

    function sendDnsResponse({ requestEth, requestIp, requestUdp, requestDns }) {
      const answerIp = dnsA[requestDns.name] ?? gatewayIp;
      const dnsPayload = encodeDnsResponseA({
        id: requestDns.id,
        name: requestDns.name,
        ip: ipToBuffer(answerIp),
      });
      const udpPayload = encodeUDP({
        srcPort: 53,
        dstPort: requestUdp.srcPort,
        payload: dnsPayload,
        srcIp: gatewayIpBuf,
        dstIp: requestIp.srcIp,
      });
      const ipPayload = encodeIPv4({
        srcIp: gatewayIpBuf,
        dstIp: requestIp.srcIp,
        protocol: 17,
        payload: udpPayload,
      });
      sendEthernet({
        dstMac: requestEth.srcMac,
        srcMac: gatewayMacBuf,
        ethertype: ETHERTYPE_IPV4,
        payload: ipPayload,
      });
    }

    function tcpKey({ clientIp, clientPort, dstIp, dstPort }) {
      return `${clientIp}:${clientPort}->${dstIp}:${dstPort}`;
    }

    function sendTcpSegment(conn, { flags, payload = Buffer.alloc(0) }) {
      const maxPayload = 1200;
      // For SYN-ACK / pure ACK segments we still need to emit a packet (payload len 0).
      if (payload.length === 0) {
        const seq =
          (flags & TCP_FLAGS.SYN) === TCP_FLAGS.SYN ? conn.serverIsn : conn.nextServerSeq;
        const tcpPayload = encodeTCP({
          srcPort: conn.dstPort,
          dstPort: conn.clientPort,
          seq,
          ack: conn.nextClientSeq,
          flags,
          payload,
          srcIp: conn.dstIpBuf,
          dstIp: conn.clientIpBuf,
        });
        const ipPayload = encodeIPv4({
          srcIp: conn.dstIpBuf,
          dstIp: conn.clientIpBuf,
          protocol: 6,
          payload: tcpPayload,
        });
        sendEthernet({
          dstMac: conn.clientMacBuf,
          srcMac: gatewayMacBuf,
          ethertype: ETHERTYPE_IPV4,
          payload: ipPayload,
        });
        return;
      }

      for (let off = 0; off < payload.length; off += maxPayload) {
        const chunk = payload.subarray(off, off + maxPayload);
        const tcpPayload = encodeTCP({
          srcPort: conn.dstPort,
          dstPort: conn.clientPort,
          seq: conn.nextServerSeq,
          ack: conn.nextClientSeq,
          flags,
          payload: chunk,
          srcIp: conn.dstIpBuf,
          dstIp: conn.clientIpBuf,
        });
        const ipPayload = encodeIPv4({
          srcIp: conn.dstIpBuf,
          dstIp: conn.clientIpBuf,
          protocol: 6,
          payload: tcpPayload,
        });
        sendEthernet({
          dstMac: conn.clientMacBuf,
          srcMac: gatewayMacBuf,
          ethertype: ETHERTYPE_IPV4,
          payload: ipPayload,
        });
        conn.nextServerSeq = (conn.nextServerSeq + chunk.length) >>> 0;
      }
    }

    function createTcpConn({ eth, ip, tcp }) {
      const clientIp = bufferToIp(ip.srcIp);
      const dstIp = bufferToIp(ip.dstIp);
      const key = tcpKey({
        clientIp,
        clientPort: tcp.srcPort,
        dstIp,
        dstPort: tcp.dstPort,
      });
      if (tcpConns.has(key)) return tcpConns.get(key);

      const forward = tcpForwardMap.get(tcp.dstPort);
      if (!forward) return null;

      const conn = {
        key,
        state: "SYN_RCVD",
        clientMacBuf: Buffer.from(eth.srcMac),
        clientIpBuf: Buffer.from(ip.srcIp),
        dstIpBuf: Buffer.from(ip.dstIp),
        clientPort: tcp.srcPort,
        dstPort: tcp.dstPort,
        nextClientSeq: (tcp.seq + 1) >>> 0,
        serverIsn: randomInt(0, 0xffffffff) >>> 0,
        nextServerSeq: 0,
        socket: null,
      };
      conn.nextServerSeq = (conn.serverIsn + 1) >>> 0;

      let socket;
      try {
        const dial = typeof createTcpConnection === "function" ? createTcpConnection : net.createConnection;
        socket = dial({
          host: forward.host,
          port: forward.port,
        });
        socket.setNoDelay?.(true);
      } catch {
        return null;
      }
      conn.socket = socket;

      socket.on("data", (data) => {
        // Only start sending once the guest handshake completed; otherwise buffer
        // could be required. For this prototype, the echo server only sends after
        // receiving data, so this is safe.
        sendTcpSegment(conn, { flags: TCP_FLAGS.PSH | TCP_FLAGS.ACK, payload: data });
      });
      socket.on("error", () => {
        // For a minimal prototype, ignore the details but clean up resources.
        tcpConns.delete(key);
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      });
      socket.on("close", () => {
        tcpConns.delete(key);
      });

      tcpConns.set(key, conn);
      return conn;
    }

    ws.on("message", (msg) => {
      const raw = Buffer.isBuffer(msg) ? msg : Buffer.from(msg);
      let decoded;
      try {
        decoded = decodeL2Message(raw);
      } catch {
        return;
      }

      if (decoded.type === L2_TUNNEL_TYPE_PING) {
        wsSendSafe(ws, encodePong(decoded.payload));
        return;
      }

      if (decoded.type !== L2_TUNNEL_TYPE_FRAME) return;

      const frame = decoded.payload;
      let eth;
      try {
        eth = parseEthernetFrame(frame);
      } catch {
        return;
      }

      if (eth.ethertype === ETHERTYPE_ARP) {
        const arp = parseArp(eth.payload);
        if (arp.opcode !== 1) return;
        if (!arp.targetIp.equals(gatewayIpBuf)) return;
        sendArpReply({ requestEth: eth, requestArp: arp });
        return;
      }

      if (eth.ethertype !== ETHERTYPE_IPV4) return;
      const ip = parseIPv4(eth.payload);

      if (ip.protocol === 17) {
        const udp = parseUDP(ip.payload);
        if (udp.dstPort !== 53) return;
        if (!ip.dstIp.equals(gatewayIpBuf)) return;
        const dns = parseDnsQuery(udp.payload);
        sendDnsResponse({ requestEth: eth, requestIp: ip, requestUdp: udp, requestDns: dns });
        return;
      }

      if (ip.protocol !== 6) return;
      const tcp = parseTCP(ip.payload);

      const synOnly = (tcp.flags & (TCP_FLAGS.SYN | TCP_FLAGS.ACK)) === TCP_FLAGS.SYN;
      if (synOnly) {
        const conn = createTcpConn({ eth, ip, tcp });
        if (!conn) return;
        sendTcpSegment(conn, { flags: TCP_FLAGS.SYN | TCP_FLAGS.ACK });
        return;
      }

      const key = tcpKey({
        clientIp: bufferToIp(ip.srcIp),
        clientPort: tcp.srcPort,
        dstIp: bufferToIp(ip.dstIp),
        dstPort: tcp.dstPort,
      });
      const conn = tcpConns.get(key);
      if (!conn) return;

      // Handshake completion.
      if (conn.state === "SYN_RCVD") {
        const isAck = (tcp.flags & TCP_FLAGS.ACK) === TCP_FLAGS.ACK;
        if (isAck && tcp.ack === conn.nextServerSeq) {
          conn.state = "ESTABLISHED";
        }
      }

      // Data from client.
      if (conn.state === "ESTABLISHED" && tcp.payload.length > 0) {
        // This prototype is intentionally strict: ignore out-of-order segments.
        if (tcp.seq !== conn.nextClientSeq) return;
        conn.nextClientSeq = (conn.nextClientSeq + tcp.payload.length) >>> 0;
        try {
          conn.socket.write(tcp.payload);
        } catch {
          try {
            conn.socket.destroy();
          } catch {
            // ignore
          }
          tcpConns.delete(key);
          return;
        }

        // Defensive: enforce a hard cap on buffered bytes in case the forwarded TCP peer
        // stops reading (or a custom socket buffers without applying backpressure).
        if (socketWritableLengthExceedsCap(conn.socket, maxTcpBufferedBytesPerConn)) {
          sendTcpSegment(conn, { flags: TCP_FLAGS.RST | TCP_FLAGS.ACK });
          try {
            conn.socket.destroy();
          } catch {
            // ignore
          }
          tcpConns.delete(key);
          return;
        }

        // ACK the data immediately.
        sendTcpSegment(conn, { flags: TCP_FLAGS.ACK });
      }

      // ACKs for server data: tracked only to avoid unbounded buffering in a real stack.
      // For the prototype, we ignore.
    });

    ws.on("close", () => {
      for (const conn of tcpConns.values()) {
        try {
          conn.socket.destroy();
        } catch {
          // ignore
        }
      }
      tcpConns.clear();
    });
  });

  async function close() {
    await new Promise((resolve) => wss.close(resolve));
  }

  return { url, close };
}

export { startProxyServer };
