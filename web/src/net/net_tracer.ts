import {
  PCAPNG_EPB_DIR_INBOUND,
  PCAPNG_EPB_DIR_OUTBOUND,
  PCAPNG_LINKTYPE_ETHERNET,
  PCAPNG_LINKTYPE_USER0,
  PCAPNG_LINKTYPE_USER1,
  writePcapng,
  type PcapngEnhancedPacket,
  type PcapngInterfaceDescription,
} from "./pcapng";

export type FrameDirection = "guest_tx" | "guest_rx";
export type ProxyDirection = "guest_to_remote" | "remote_to_guest";
export type UdpProxyTransport = "webrtc" | "proxy";

export interface NetTraceConfig {
  // Hard cap on total captured payload bytes (not including PCAPNG overhead).
  // When exceeded, new frames are dropped.
  maxBytes?: number;
  captureEthernet?: boolean;
  captureTcpProxy?: boolean;
  captureUdpProxy?: boolean;
}

type EthernetRecord = Readonly<{
  type: "ethernet";
  direction: FrameDirection;
  frame: Uint8Array<ArrayBuffer>;
  timestampNs: bigint;
}>;

type TcpProxyRecord = Readonly<{
  type: "tcp_proxy";
  direction: ProxyDirection;
  connectionId: number;
  data: Uint8Array<ArrayBuffer>;
  timestampNs: bigint;
}>;

type UdpProxyRecord = Readonly<{
  type: "udp_proxy";
  direction: ProxyDirection;
  transport: UdpProxyTransport;
  remoteIpV4: [number, number, number, number];
  srcPort: number;
  dstPort: number;
  data: Uint8Array<ArrayBuffer>;
  timestampNs: bigint;
}>;

type TraceRecord = EthernetRecord | TcpProxyRecord | UdpProxyRecord;

const DEFAULT_MAX_BYTES = 16 * 1024 * 1024;
const PROXY_PSEUDO_HEADER_LEN = 16;

function defaultTimestampNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

function proxyDirectionToPcapngFlags(direction: ProxyDirection): number {
  return direction === "guest_to_remote" ? PCAPNG_EPB_DIR_OUTBOUND : PCAPNG_EPB_DIR_INBOUND;
}

function tcpProxyPseudoPacket(connectionId: number, direction: ProxyDirection, data: Uint8Array<ArrayBuffer>): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(PROXY_PSEUDO_HEADER_LEN + data.byteLength) as Uint8Array<ArrayBuffer>;
  buf.set([0x41, 0x54, 0x43, 0x50], 0); // "ATCP"
  buf[4] = direction === "guest_to_remote" ? 0 : 1;
  // 3 bytes padding.
  buf[5] = 0;
  buf[6] = 0;
  buf[7] = 0;

  // Match the Rust tracer: `connection_id` is logically a u32, but encoded as
  // a little-endian u64 (low 32 bits = id, high 32 bits = 0).
  //
  // Avoid `BigInt(connectionId)` so bogus inputs (NaN/float) don't break
  // capture export; also keeps output deterministic under Node strip-types.
  const id = connectionId >>> 0;
  const view = new DataView(buf.buffer);
  view.setUint32(8, id, true);
  view.setUint32(12, 0, true);

  buf.set(data, PROXY_PSEUDO_HEADER_LEN);
  return buf;
}

function udpProxyPseudoPacket(
  direction: ProxyDirection,
  transport: UdpProxyTransport,
  remoteIpV4: [number, number, number, number],
  srcPort: number,
  dstPort: number,
  data: Uint8Array<ArrayBuffer>,
): Uint8Array<ArrayBuffer> {
  const buf = new Uint8Array(PROXY_PSEUDO_HEADER_LEN + data.byteLength) as Uint8Array<ArrayBuffer>;
  buf.set([0x41, 0x55, 0x44, 0x50], 0); // "AUDP"
  buf[4] = direction === "guest_to_remote" ? 0 : 1;
  buf[5] = transport === "webrtc" ? 0 : 1;
  // 2 bytes padding.
  buf[6] = 0;
  buf[7] = 0;
  buf.set(remoteIpV4, 8);

  const view = new DataView(buf.buffer);
  view.setUint16(12, srcPort & 0xffff, true);
  view.setUint16(14, dstPort & 0xffff, true);

  buf.set(data, PROXY_PSEUDO_HEADER_LEN);
  return buf;
}

export class NetTracer {
  private enabled = false;

  private readonly maxBytes: number;
  private readonly captureEthernet: boolean;
  private readonly captureTcpProxy: boolean;
  private readonly captureUdpProxy: boolean;

  private records: TraceRecord[] = [];
  private bytes = 0;
  private droppedRecords = 0;
  private droppedBytes = 0;

  constructor(cfg: NetTraceConfig = {}) {
    const maxBytes = cfg.maxBytes ?? DEFAULT_MAX_BYTES;
    this.maxBytes = Number.isFinite(maxBytes) && maxBytes >= 0 ? maxBytes : DEFAULT_MAX_BYTES;
    this.captureEthernet = cfg.captureEthernet ?? true;
    // Match Rust defaults: proxy payload capture is opt-in (more sensitive than
    // raw Ethernet framing).
    this.captureTcpProxy = cfg.captureTcpProxy ?? false;
    this.captureUdpProxy = cfg.captureUdpProxy ?? false;
  }

  enable(): void {
    this.enabled = true;
  }

  disable(): void {
    this.enabled = false;
  }

  isEnabled(): boolean {
    return this.enabled;
  }

  clear(): void {
    this.records = [];
    this.bytes = 0;
    this.droppedRecords = 0;
    this.droppedBytes = 0;
  }

  recordEthernet(direction: FrameDirection, frame: Uint8Array, timestampNs: bigint = defaultTimestampNs()): void {
    if (!this.enabled) return;
    if (!this.captureEthernet) return;

    const len = frame.byteLength;
    if (len === 0) return;
    if (len > this.maxBytes || this.bytes + len > this.maxBytes) {
      this.droppedRecords += 1;
      this.droppedBytes += len;
      return;
    }

    // Always copy: guest ring buffers are often SharedArrayBuffer-backed and/or
    // reused by the producer.
    const copied = new Uint8Array(frame) as Uint8Array<ArrayBuffer>;
    this.records.push({ type: "ethernet", direction, frame: copied, timestampNs });
    this.bytes += len;
  }

  recordTcpProxy(
    direction: ProxyDirection,
    connectionId: number,
    data: Uint8Array,
    timestampNs: bigint = defaultTimestampNs(),
  ): void {
    if (!this.enabled) return;
    if (!this.captureTcpProxy) return;

    const dataLen = data.byteLength;
    const len = PROXY_PSEUDO_HEADER_LEN + dataLen;
    if (len > this.maxBytes || this.bytes + len > this.maxBytes) {
      this.droppedRecords += 1;
      this.droppedBytes += len;
      return;
    }

    const copied = new Uint8Array(data) as Uint8Array<ArrayBuffer>;
    this.records.push({ type: "tcp_proxy", direction, connectionId: connectionId >>> 0, data: copied, timestampNs });
    this.bytes += len;
  }

  recordUdpProxy(
    direction: ProxyDirection,
    transport: UdpProxyTransport,
    remoteIpV4: [number, number, number, number],
    srcPort: number,
    dstPort: number,
    data: Uint8Array,
    timestampNs: bigint = defaultTimestampNs(),
  ): void {
    if (!this.enabled) return;
    if (!this.captureUdpProxy) return;

    const dataLen = data.byteLength;
    const len = PROXY_PSEUDO_HEADER_LEN + dataLen;
    if (len > this.maxBytes || this.bytes + len > this.maxBytes) {
      this.droppedRecords += 1;
      this.droppedBytes += len;
      return;
    }

    const copied = new Uint8Array(data) as Uint8Array<ArrayBuffer>;
    const ip: [number, number, number, number] = [
      remoteIpV4[0] & 0xff,
      remoteIpV4[1] & 0xff,
      remoteIpV4[2] & 0xff,
      remoteIpV4[3] & 0xff,
    ];
    this.records.push({
      type: "udp_proxy",
      direction,
      transport,
      remoteIpV4: ip,
      srcPort: srcPort & 0xffff,
      dstPort: dstPort & 0xffff,
      data: copied,
      timestampNs,
    });
    this.bytes += len;
  }

  takePcapng(): Uint8Array<ArrayBuffer> {
    const out = this.exportPcapng();
    this.records = [];
    this.bytes = 0;
    return out;
  }

  exportPcapng(): Uint8Array<ArrayBuffer> {
    const hasTcpProxy = this.records.some((r) => r.type === "tcp_proxy");
    const hasUdpProxy = this.records.some((r) => r.type === "udp_proxy");

    // Mirror the Rust tracer: a single Ethernet interface with direction encoded
    // via `epb_flags`, plus optional pseudo-interfaces for proxy traffic.
    const interfaces: PcapngInterfaceDescription[] = [
      { linkType: PCAPNG_LINKTYPE_ETHERNET, snapLen: 0xffff, name: "guest-eth0", tsResolPower10: 9 },
    ];

    const tcpProxyInterfaceId = hasTcpProxy ? interfaces.length : null;
    if (tcpProxyInterfaceId !== null) {
      interfaces.push({ linkType: PCAPNG_LINKTYPE_USER0, snapLen: 0xffff, name: "tcp-proxy", tsResolPower10: 9 });
    }

    const udpProxyInterfaceId = hasUdpProxy ? interfaces.length : null;
    if (udpProxyInterfaceId !== null) {
      interfaces.push({ linkType: PCAPNG_LINKTYPE_USER1, snapLen: 0xffff, name: "udp-proxy", tsResolPower10: 9 });
    }

    const packets: PcapngEnhancedPacket[] = [];
    for (const rec of this.records) {
      switch (rec.type) {
        case "ethernet":
          packets.push({
            interfaceId: 0,
            timestamp: rec.timestampNs,
            packet: rec.frame,
            // Direction is encoded via the Enhanced Packet Block `epb_flags` option.
            flags: rec.direction === "guest_rx" ? PCAPNG_EPB_DIR_INBOUND : PCAPNG_EPB_DIR_OUTBOUND,
          });
          break;
        case "tcp_proxy":
          if (tcpProxyInterfaceId === null) break;
          packets.push({
            interfaceId: tcpProxyInterfaceId,
            timestamp: rec.timestampNs,
            packet: tcpProxyPseudoPacket(rec.connectionId, rec.direction, rec.data),
            flags: proxyDirectionToPcapngFlags(rec.direction),
          });
          break;
        case "udp_proxy":
          if (udpProxyInterfaceId === null) break;
          packets.push({
            interfaceId: udpProxyInterfaceId,
            timestamp: rec.timestampNs,
            packet: udpProxyPseudoPacket(rec.direction, rec.transport, rec.remoteIpV4, rec.srcPort, rec.dstPort, rec.data),
            flags: proxyDirectionToPcapngFlags(rec.direction),
          });
          break;
      }
    }

    return writePcapng({ interfaces, packets });
  }

  stats(): { enabled: boolean; records: number; bytes: number; droppedRecords: number; droppedBytes: number } {
    return {
      enabled: this.enabled,
      records: this.records.length,
      bytes: this.bytes,
      droppedRecords: this.droppedRecords,
      droppedBytes: this.droppedBytes,
    };
  }
}
