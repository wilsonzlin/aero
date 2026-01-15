import dgram from "node:dgram";
import ipaddr from "ipaddr.js";
import { type WebSocket } from "ws";

import type { ProxyConfig } from "./config";
import type { ProxyServerMetrics } from "./metrics";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";
import { wsCloseSafe } from "./wsClose";
import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "./udpRelayProtocol";
import { stripIpv6ZoneIndex } from "./ipUtils";

export async function handleUdpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  if (ws.readyState !== ws.OPEN) return;

  const socket = dgram.createSocket(family === 6 ? "udp6" : "udp4");
  socket.connect(port, address);

  metrics.connectionActiveInc("udp");

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");
    try {
      socket.close();
    } catch {
      // ignore
    }

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");
    try {
      socket.close();
    } catch {
      // ignore
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("error", (err) => {
    closeBoth("udp_error", 1011, "UDP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("message", (msg) => {
    bytesOut += msg.length;
    metrics.addBytesOut("udp", msg.length);
    if (ws.readyState !== ws.OPEN) return;

    if (ws.bufferedAmount > config.udpWsBufferedAmountLimitBytes) {
      log("warn", "udp_drop_backpressure", {
        connId,
        bufferedAmount: ws.bufferedAmount,
        limit: config.udpWsBufferedAmountLimitBytes,
        droppedBytes: msg.length
      });
      return;
    }

    ws.send(msg);
  });

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);
    bytesIn += buf.length;
    metrics.addBytesIn("udp", buf.length);
    socket.send(buf);
  });
}

type UdpRelayBindingKey = `${4 | 6}:${number}`;

interface UdpRelayBinding {
  key: UdpRelayBindingKey;
  guestPort: number;
  addressFamily: 4 | 6;
  socket: dgram.Socket;
  lastActiveMs: number;
  allowedRemotes: Map<string, number>;
  lastAllowedPruneMs: number;
}

function makeUdpRelayBindingKey(guestPort: number, addressFamily: 4 | 6): UdpRelayBindingKey {
  return `${addressFamily}:${guestPort}`;
}

export async function handleUdpRelayMultiplexed(
  ws: WebSocket,
  connId: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  const bindings = new Map<UdpRelayBindingKey, UdpRelayBinding>();
  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;
  let gcTimer: NodeJS.Timeout | null = null;
  let clientSupportsV2 = false;

  metrics.connectionActiveInc("udp");

  const remoteAllowlistEnabled = config.udpRelayInboundFilterMode === "address_and_port";
  const remoteAllowlistIdleTimeoutMs = config.udpRelayBindingIdleTimeoutMs;
  const maxAllowedRemotesBeforePrune = 1024;

  const remoteKey = (ipBytes: Uint8Array, port: number): string => `${Buffer.from(ipBytes).toString("hex")}:${port}`;

  const pruneAllowedRemotes = (binding: UdpRelayBinding, now: number) => {
    if (!remoteAllowlistEnabled) return;
    if (remoteAllowlistIdleTimeoutMs > 0) {
      if (
        binding.allowedRemotes.size <= maxAllowedRemotesBeforePrune &&
        binding.lastAllowedPruneMs !== 0 &&
        now - binding.lastAllowedPruneMs <= remoteAllowlistIdleTimeoutMs
      ) {
        return;
      }

      const cutoff = now - remoteAllowlistIdleTimeoutMs;
      for (const [key, ts] of binding.allowedRemotes) {
        if (ts < cutoff) {
          binding.allowedRemotes.delete(key);
        }
      }
      binding.lastAllowedPruneMs = now;
      return;
    }

    // No idle timeout: still cap memory growth.
    if (binding.allowedRemotes.size > maxAllowedRemotesBeforePrune) {
      binding.allowedRemotes.clear();
      binding.lastAllowedPruneMs = now;
    }
  };

  const allowRemote = (binding: UdpRelayBinding, ipBytes: Uint8Array, port: number, now: number) => {
    if (!remoteAllowlistEnabled) return;
    pruneAllowedRemotes(binding, now);
    binding.allowedRemotes.set(remoteKey(ipBytes, port), now);
  };

  const remoteAllowed = (binding: UdpRelayBinding, ipBytes: Uint8Array, port: number, now: number): boolean => {
    if (!remoteAllowlistEnabled) return true;
    const key = remoteKey(ipBytes, port);
    const last = binding.allowedRemotes.get(key);
    if (last === undefined) return false;

    if (remoteAllowlistIdleTimeoutMs > 0 && now - last > remoteAllowlistIdleTimeoutMs) {
      binding.allowedRemotes.delete(key);
      return false;
    }

    // Refresh timestamp to keep active flows alive.
    binding.allowedRemotes.set(key, now);
    return true;
  };

  const closeAll = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");

    if (gcTimer) {
      clearInterval(gcTimer);
      gcTimer = null;
    }

    const bindingCount = bindings.size;
    if (bindingCount > 0) {
      metrics.udpBindingsActiveDec(bindingCount);
    }
    for (const binding of bindings.values()) {
      try {
        binding.socket.close();
      } catch {
        // ignore
      }
    }
    bindings.clear();

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      mode: "multiplexed",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");

    if (gcTimer) {
      clearInterval(gcTimer);
      gcTimer = null;
    }

    const bindingCount = bindings.size;
    if (bindingCount > 0) {
      metrics.udpBindingsActiveDec(bindingCount);
    }
    for (const binding of bindings.values()) {
      try {
        binding.socket.close();
      } catch {
        // ignore
      }
    }
    bindings.clear();

    log("info", "conn_close", {
      connId,
      proto: "udp",
      mode: "multiplexed",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeAll("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", mode: "multiplexed", err: formatError(err) });
  });

  if (config.udpRelayBindingIdleTimeoutMs > 0) {
    const gcIntervalMs = Math.max(1_000, Math.min(10_000, Math.floor(config.udpRelayBindingIdleTimeoutMs / 2)));
    gcTimer = setInterval(() => {
      if (closed) return;
      const now = Date.now();
      for (const [key, binding] of bindings) {
        if (now - binding.lastActiveMs <= config.udpRelayBindingIdleTimeoutMs) continue;
        bindings.delete(key);
        metrics.udpBindingsActiveDec();
        try {
          binding.socket.close();
        } catch {
          // ignore
        }
      }
    }, gcIntervalMs);
    gcTimer.unref();
  }

  const getOrCreateBinding = (guestPort: number, addressFamily: 4 | 6): UdpRelayBinding | null => {
    if (closed) return null;
    const key = makeUdpRelayBindingKey(guestPort, addressFamily);
    const existing = bindings.get(key);
    if (existing) return existing;

    if (bindings.size >= config.udpRelayMaxBindingsPerConnection) {
      closeAll("udp_max_bindings", 1008, "Too many UDP bindings");
      return null;
    }

    const socket = dgram.createSocket(addressFamily === 6 ? "udp6" : "udp4");
    const binding: UdpRelayBinding = {
      key,
      guestPort,
      addressFamily,
      socket,
      lastActiveMs: Date.now(),
      allowedRemotes: new Map(),
      lastAllowedPruneMs: 0
    };

    socket.on("error", (err) => {
      metrics.incConnectionError("error");
      log("error", "connect_error", { connId, proto: "udp", mode: "multiplexed", err: formatError(err), guestPort, addressFamily });
      const removed = bindings.delete(key);
      if (removed) {
        metrics.udpBindingsActiveDec();
      }
      try {
        socket.close();
      } catch {
        // ignore
      }
    });

    socket.on("message", (msg, rinfo) => {
      const now = Date.now();
      binding.lastActiveMs = now;

      if (msg.length > config.udpRelayMaxPayloadBytes) return;
      if (ws.readyState !== ws.OPEN) return;

      let frame: Uint8Array;
      try {
        const addr = stripIpv6ZoneIndex(rinfo.address);
        const parsed = ipaddr.parse(addr);
        const ipBytes = new Uint8Array(parsed.toByteArray());

        if (addressFamily === 4) {
          if (ipBytes.length !== 4) return;
          if (!remoteAllowed(binding, ipBytes, rinfo.port, now)) return;
          if (config.udpRelayPreferV2 && clientSupportsV2) {
            frame = encodeUdpRelayV2Datagram(
              {
                guestPort,
                remoteIp: ipBytes,
                remotePort: rinfo.port,
                payload: msg
              },
              { maxPayload: config.udpRelayMaxPayloadBytes }
            );
          } else {
            frame = encodeUdpRelayV1Datagram(
              {
                guestPort,
                remoteIpv4: [ipBytes[0]!, ipBytes[1]!, ipBytes[2]!, ipBytes[3]!],
                remotePort: rinfo.port,
                payload: msg
              },
              { maxPayload: config.udpRelayMaxPayloadBytes }
            );
          }
        } else {
          if (ipBytes.length !== 16) return;
          if (!remoteAllowed(binding, ipBytes, rinfo.port, now)) return;
          frame = encodeUdpRelayV2Datagram(
            {
              guestPort,
              remoteIp: ipBytes,
              remotePort: rinfo.port,
              payload: msg
            },
            { maxPayload: config.udpRelayMaxPayloadBytes }
          );
        }
      } catch {
        return;
      }

      bytesOut += msg.length;
      metrics.addBytesOut("udp", msg.length);

      if (ws.bufferedAmount > config.udpWsBufferedAmountLimitBytes) {
        log("warn", "udp_drop_backpressure", {
          connId,
          bufferedAmount: ws.bufferedAmount,
          limit: config.udpWsBufferedAmountLimitBytes,
          droppedBytes: frame.length
        });
        return;
      }

      ws.send(frame);
    });

    bindings.set(key, binding);
    metrics.udpBindingsActiveInc();
    return binding;
  };

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);

    void (async () => {
      if (closed) return;
      let guestPort: number;
      let remotePort: number;
      let addressFamily: 4 | 6;
      let remoteIpBytes: Uint8Array;
      let payload: Uint8Array;

      try {
        const decoded = decodeUdpRelayFrame(buf, { maxPayload: config.udpRelayMaxPayloadBytes });
        if (decoded.version === 1) {
          guestPort = decoded.guestPort;
          remotePort = decoded.remotePort;
          addressFamily = 4;
          remoteIpBytes = Uint8Array.from(decoded.remoteIpv4);
          payload = decoded.payload;
        } else {
          clientSupportsV2 = true;
          guestPort = decoded.guestPort;
          remotePort = decoded.remotePort;
          addressFamily = decoded.addressFamily;
          remoteIpBytes = decoded.remoteIp;
          payload = decoded.payload;
        }
      } catch {
        return;
      }

      if (closed) return;
      if (remotePort < 1 || remotePort > 65535) return;
      if (payload.length > config.udpRelayMaxPayloadBytes) return;

      let remoteAddress: string;
      try {
        remoteAddress = ipaddr.fromByteArray(Array.from(remoteIpBytes)).toString();
      } catch {
        return;
      }

      let decision;
      try {
        decision = await resolveAndAuthorizeTarget(remoteAddress, remotePort, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });
      } catch (err) {
        metrics.incConnectionError("error");
        log("error", "connect_error", {
          connId,
          proto: "udp",
          mode: "multiplexed",
          err: formatError(err),
          remoteAddress,
          remotePort,
          guestPort
        });
        closeAll("policy_error", 1011, "Proxy error");
        return;
      }
      if (closed) return;
      if (!decision.allowed) {
        metrics.incConnectionError("denied");
        return;
      }

      if (addressFamily === 4 && decision.target.family !== 4) return;
      if (addressFamily === 6 && decision.target.family !== 6) return;

      const binding = getOrCreateBinding(guestPort, addressFamily);
      if (!binding) return;
      const now = Date.now();
      binding.lastActiveMs = now;
      allowRemote(binding, remoteIpBytes, remotePort, now);

      bytesIn += payload.length;
      metrics.addBytesIn("udp", payload.length);

      // Send the raw UDP payload to the decoded destination.
      try {
        binding.socket.send(payload, remotePort, remoteAddress);
      } catch {
        // ignore
      }
    })();
  });
}

