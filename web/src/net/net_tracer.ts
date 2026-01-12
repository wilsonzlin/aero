import {
  PCAPNG_EPB_DIR_INBOUND,
  PCAPNG_EPB_DIR_OUTBOUND,
  PCAPNG_LINKTYPE_ETHERNET,
  writePcapng,
  type PcapngEnhancedPacket,
  type PcapngInterfaceDescription,
} from "./pcapng";

export type FrameDirection = "guest_tx" | "guest_rx";

export interface NetTraceConfig {
  // Hard cap on total captured payload bytes (not including PCAPNG overhead).
  // When exceeded, new frames are dropped.
  maxBytes?: number;
  captureEthernet?: boolean;
}

type EthernetRecord = Readonly<{
  direction: FrameDirection;
  frame: Uint8Array<ArrayBuffer>;
  timestampNs: bigint;
}>;

const DEFAULT_MAX_BYTES = 16 * 1024 * 1024;

function defaultTimestampNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

export class NetTracer {
  private enabled = false;

  private readonly maxBytes: number;
  private readonly captureEthernet: boolean;

  private records: EthernetRecord[] = [];
  private bytes = 0;
  private droppedRecords = 0;
  private droppedBytes = 0;

  constructor(cfg: NetTraceConfig = {}) {
    const maxBytes = cfg.maxBytes ?? DEFAULT_MAX_BYTES;
    this.maxBytes = Number.isFinite(maxBytes) && maxBytes >= 0 ? maxBytes : DEFAULT_MAX_BYTES;
    this.captureEthernet = cfg.captureEthernet ?? true;
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
    this.records.push({ direction, frame: copied, timestampNs });
    this.bytes += len;
  }

  takePcapng(): Uint8Array<ArrayBuffer> {
    const out = this.exportPcapng();
    this.records = [];
    this.bytes = 0;
    return out;
  }

  exportPcapng(): Uint8Array<ArrayBuffer> {
    if (!this.captureEthernet) {
      // Still emit a valid empty PCAPNG.
      return writePcapng({ interfaces: [], packets: [] });
    }

    const interfaces: PcapngInterfaceDescription[] = [
      { linkType: PCAPNG_LINKTYPE_ETHERNET, snapLen: 0xffff, name: "guest_rx", tsResolPower10: 9 },
      { linkType: PCAPNG_LINKTYPE_ETHERNET, snapLen: 0xffff, name: "guest_tx", tsResolPower10: 9 },
    ];

    const packets: PcapngEnhancedPacket[] = this.records.map((rec) => ({
      interfaceId: rec.direction === "guest_rx" ? 0 : 1,
      timestamp: rec.timestampNs,
      packet: rec.frame,
      // Also set EPB direction flags for compatibility with readers that use
      // `epb_flags` instead of (or in addition to) the interface list.
      flags: rec.direction === "guest_rx" ? PCAPNG_EPB_DIR_INBOUND : PCAPNG_EPB_DIR_OUTBOUND,
    }));

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
