import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { EventEmitter } from "node:events";

import { unrefBestEffort } from "../src/unref_safe.js";

import { startProxyServer } from "../prototype/nt-arch-rfc/proxy-server.js";
import { runNetworkingProbe } from "../prototype/nt-arch-rfc/client.js";
import WebSocket from "../tools/minimal_ws.js";
import { wsSendSafe } from "../scripts/_shared/ws_safe.js";
import {
  TCP_FLAGS,
  encodeEthernetFrame,
  encodeIPv4,
  encodeTCP,
  ipToBuffer,
  macToBuffer,
  parseEthernetFrame,
  parseIPv4,
  parseTCP,
} from "../prototype/nt-arch-rfc/packets.js";
import { L2_TUNNEL_SUBPROTOCOL, L2_TUNNEL_TYPE_FRAME, decodeL2Message, encodeL2Frame } from "../prototype/nt-arch-rfc/l2_tunnel_proto.js";

async function startTcpEchoServer() {
  const server = net.createServer((socket) => {
    socket.on("data", (data) => {
      try {
        socket.write(data);
      } catch {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return {
    port: address.port,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

async function expectTcpRstWhenBufferedBytesCapIsExceeded({ maxTcpBufferedBytesPerConn, createTcpConnection }) {
  const guestMacBuf = macToBuffer("02:00:00:00:00:01");
  const gatewayMacBuf = macToBuffer("02:00:00:00:00:02");
  const guestIpBuf = ipToBuffer("10.0.2.15");
  const dstIpBuf = ipToBuffer("203.0.113.10");

  let createdResolve = null;
  const created = new Promise((resolve) => {
    createdResolve = resolve;
  });

  const proxy = await startProxyServer({
    tcpForward: {
      4242: { host: "127.0.0.1", port: 4242 },
    },
    maxTcpBufferedBytesPerConn,
    createTcpConnection: () => {
      createdResolve?.();
      return createTcpConnection();
    },
  });

  const frames = [];
  /** @type {Array<{ predicate: (f: Buffer) => boolean, resolve: (f: Buffer) => void, reject: (e: Error) => void, timer: any }>} */
  const waiters = [];

  const ws = new WebSocket(proxy.url, L2_TUNNEL_SUBPROTOCOL);
  ws.binaryType = "nodebuffer";
  await new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });

  try {
    ws.on("message", (msg) => {
      const buf = Buffer.isBuffer(msg) ? msg : Buffer.from(msg);
      let decoded;
      try {
        decoded = decodeL2Message(buf);
      } catch {
        return;
      }
      if (decoded.type !== L2_TUNNEL_TYPE_FRAME) return;
      const frame = Buffer.from(decoded.payload);
      const idx = waiters.findIndex((w) => w.predicate(frame));
      if (idx !== -1) {
        const w = waiters.splice(idx, 1)[0];
        clearTimeout(w.timer);
        w.resolve(frame);
        return;
      }
      frames.push(frame);
    });

    const waitFor = (predicate, timeoutMs = 2000) => {
      const bufferedIdx = frames.findIndex(predicate);
      if (bufferedIdx !== -1) return Promise.resolve(frames.splice(bufferedIdx, 1)[0]);
      return new Promise((resolve, reject) => {
        let entry;
        const timer = setTimeout(() => {
          const idx = waiters.indexOf(entry);
          if (idx !== -1) waiters.splice(idx, 1);
          reject(new Error("timeout"));
        }, timeoutMs);
        unrefBestEffort(timer);
        entry = { predicate, resolve, reject, timer };
        waiters.push(entry);
      });
    };

    const sendL2 = (eth) => {
      assert.ok(wsSendSafe(ws, encodeL2Frame(eth)));
    };

    const clientPort = 50000;
    const clientIsn = 12345;
    const syn = encodeTCP({
      srcPort: clientPort,
      dstPort: 4242,
      seq: clientIsn,
      ack: 0,
      flags: TCP_FLAGS.SYN,
      srcIp: guestIpBuf,
      dstIp: dstIpBuf,
    });
    sendL2(
      encodeEthernetFrame({
        dstMac: gatewayMacBuf,
        srcMac: guestMacBuf,
        ethertype: 0x0800,
        payload: encodeIPv4({ srcIp: guestIpBuf, dstIp: dstIpBuf, protocol: 6, payload: syn }),
      }),
    );

    const synAckFrame = await waitFor((frame) => {
      try {
        const eth = parseEthernetFrame(frame);
        const ip = parseIPv4(eth.payload);
        const tcp = parseTCP(ip.payload);
        const want = TCP_FLAGS.SYN | TCP_FLAGS.ACK;
        return tcp.dstPort === clientPort && tcp.srcPort === 4242 && (tcp.flags & want) === want;
      } catch {
        return false;
      }
    });
    const synAckTcp = (() => {
      const eth = parseEthernetFrame(synAckFrame);
      const ip = parseIPv4(eth.payload);
      return parseTCP(ip.payload);
    })();

    await created;

    const serverIsn = synAckTcp.seq >>> 0;
    const nextClientSeq = (clientIsn + 1) >>> 0;
    const nextServerSeq = (serverIsn + 1) >>> 0;

    const ack = encodeTCP({
      srcPort: clientPort,
      dstPort: 4242,
      seq: nextClientSeq,
      ack: nextServerSeq,
      flags: TCP_FLAGS.ACK,
      srcIp: guestIpBuf,
      dstIp: dstIpBuf,
    });
    sendL2(
      encodeEthernetFrame({
        dstMac: gatewayMacBuf,
        srcMac: guestMacBuf,
        ethertype: 0x0800,
        payload: encodeIPv4({ srcIp: guestIpBuf, dstIp: dstIpBuf, protocol: 6, payload: ack }),
      }),
    );

    const data = encodeTCP({
      srcPort: clientPort,
      dstPort: 4242,
      seq: nextClientSeq,
      ack: nextServerSeq,
      flags: TCP_FLAGS.PSH | TCP_FLAGS.ACK,
      payload: Buffer.from("a"),
      srcIp: guestIpBuf,
      dstIp: dstIpBuf,
    });
    sendL2(
      encodeEthernetFrame({
        dstMac: gatewayMacBuf,
        srcMac: guestMacBuf,
        ethertype: 0x0800,
        payload: encodeIPv4({ srcIp: guestIpBuf, dstIp: dstIpBuf, protocol: 6, payload: data }),
      }),
    );

    const rstFrame = await waitFor((frame) => {
      try {
        const eth = parseEthernetFrame(frame);
        const ip = parseIPv4(eth.payload);
        const tcp = parseTCP(ip.payload);
        return tcp.dstPort === clientPort && tcp.srcPort === 4242 && (tcp.flags & TCP_FLAGS.RST) === TCP_FLAGS.RST;
      } catch {
        return false;
      }
    });
    assert.ok(rstFrame);
  } finally {
    try {
      ws.close();
    } catch {
      // ignore
    }
    await proxy.close();
  }
}

test("Networking architecture RFC prototype: ARP + DNS + TCP echo over L2 tunnel", async () => {
  const echo = await startTcpEchoServer();
  const proxy = await startProxyServer({
    tcpForward: {
      [echo.port]: { host: "127.0.0.1", port: echo.port },
    },
    dnsA: { "echo.local": "203.0.113.10" },
  });

  try {
    const result = await runNetworkingProbe({
      url: proxy.url,
      dnsName: "echo.local",
      echoPort: echo.port,
      throughputBytes: 128 * 1024,
    });

    assert.equal(result.dns.name, "echo.local");
    assert.equal(result.dns.ip, "203.0.113.10");
    assert.ok(result.dns.rttMs > 0);

    assert.equal(result.tcp.remotePort, echo.port);
    assert.ok(result.tcp.connectRttMs > 0);
    assert.ok(result.tcp.throughputMbps > 0);
    assert.equal(result.tcp.ok, true);
  } finally {
    await proxy.close();
    await echo.close();
  }
});

test("Networking architecture RFC prototype: closes TCP conn with RST when TCP buffering cap is exceeded", async () => {
  await expectTcpRstWhenBufferedBytesCapIsExceeded({
    maxTcpBufferedBytesPerConn: 1,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        writableLength = 0;
        setNoDelay() {}
        write(_chunk) {
          // Simulate pathological buffering.
          this.writableLength += 1024;
          return true;
        }
        destroy() {
          queueMicrotask(() => this.emit("close"));
        }
      }
      return new FakeTcpSocket();
    },
  });
});

test("Networking architecture RFC prototype: closes TCP conn with RST when writableLength getter throws", async () => {
  await expectTcpRstWhenBufferedBytesCapIsExceeded({
    maxTcpBufferedBytesPerConn: 1,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        setNoDelay() {}
        write(_chunk) {
          return true;
        }
        get writableLength() {
          throw new Error("boom");
        }
        destroy() {
          queueMicrotask(() => this.emit("close"));
        }
      }
      return new FakeTcpSocket();
    },
  });
});
