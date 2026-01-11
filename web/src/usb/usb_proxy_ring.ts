import type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";

// SharedArrayBuffer-backed SPSC ring buffer for variable-length USB proxy records.
//
// This is used as an optional fast path for WebUSB passthrough proxying:
// - worker -> main thread: UsbHostAction records
// - main thread -> worker: UsbHostCompletion records
//
// Layout:
//   ctrl: Int32Array[3] (head, tail, dropped)
//   data: Uint8Array[dataCapacityBytes]
//
// `head`/`tail` are monotonic u32 byte counters (wrapping at 2^32). The actual
// byte index into `data` is `counter % dataCapacityBytes`.
//
// Records are padded to 4-byte alignment. When a record would straddle the end
// of the buffer, the producer may write a wrap marker record; the consumer then
// skips to the next wrap boundary.

export const USB_PROXY_RING_CTRL_WORDS = 3;
export const USB_PROXY_RING_CTRL_BYTES = USB_PROXY_RING_CTRL_WORDS * 4;

export const USB_PROXY_RING_MIN_HEADER_BYTES = 8;
export const USB_PROXY_RING_ALIGN = 4;

// Action records share an 8-byte header:
//   kind: u8
//   reserved: u8
//   reserved: u16
//   id: u32 (LE)
export const USB_PROXY_ACTION_HEADER_BYTES = 8;

// Completion records share an 8-byte header:
//   kind: u8
//   status: u8
//   reserved: u16
//   id: u32 (LE)
export const USB_PROXY_COMPLETION_HEADER_BYTES = 8;

const enum CtrlIndex {
  Head = 0,
  Tail = 1,
  Dropped = 2,
}

const enum UsbRecordKindTag {
  ControlIn = 1,
  ControlOut = 2,
  BulkIn = 3,
  BulkOut = 4,
  WrapMarker = 0xff,
}

const enum UsbCompletionStatusTag {
  Success = 0,
  Stall = 1,
  Error = 2,
}

const SETUP_PACKET_BYTES = 8;

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function u32(n: number): number {
  return n >>> 0;
}

function alignUp(value: number, align: number): number {
  if ((align & (align - 1)) !== 0) throw new Error("align must be power of two");
  return (value + (align - 1)) & ~(align - 1);
}

function encodeSetupPacket(view: DataView, offset: number, setup: SetupPacket): void {
  view.setUint8(offset + 0, setup.bmRequestType & 0xff);
  view.setUint8(offset + 1, setup.bRequest & 0xff);
  view.setUint16(offset + 2, setup.wValue & 0xffff, true);
  view.setUint16(offset + 4, setup.wIndex & 0xffff, true);
  view.setUint16(offset + 6, setup.wLength & 0xffff, true);
}

function decodeSetupPacket(view: DataView, offset: number): SetupPacket {
  return {
    bmRequestType: view.getUint8(offset + 0) >>> 0,
    bRequest: view.getUint8(offset + 1) >>> 0,
    wValue: view.getUint16(offset + 2, true) >>> 0,
    wIndex: view.getUint16(offset + 4, true) >>> 0,
    wLength: view.getUint16(offset + 6, true) >>> 0,
  };
}

const TRUNCATION_MARKER = " [truncated]";
const TRUNCATION_MARKER_BYTES = textEncoder.encode(TRUNCATION_MARKER);

function truncateUtf8(bytes: Uint8Array, maxBytes: number): Uint8Array {
  if (bytes.byteLength <= maxBytes) return bytes;
  if (maxBytes <= 0) return new Uint8Array();
  if (maxBytes <= TRUNCATION_MARKER_BYTES.byteLength) return TRUNCATION_MARKER_BYTES.slice(0, maxBytes);
  const head = bytes.slice(0, maxBytes - TRUNCATION_MARKER_BYTES.byteLength);
  const out = new Uint8Array(maxBytes);
  out.set(head, 0);
  out.set(TRUNCATION_MARKER_BYTES, head.byteLength);
  return out;
}

export function createUsbProxyRingBuffer(dataCapacityBytes: number): SharedArrayBuffer {
  const cap = dataCapacityBytes >>> 0;
  if (cap === 0) throw new Error("dataCapacityBytes must be > 0");
  return new SharedArrayBuffer(USB_PROXY_RING_CTRL_BYTES + cap);
}

export class UsbProxyRing {
  readonly #ctrl: Int32Array;
  readonly #data: Uint8Array;
  readonly #view: DataView;
  readonly #cap: number;

  constructor(buffer: SharedArrayBuffer) {
    this.#ctrl = new Int32Array(buffer, 0, USB_PROXY_RING_CTRL_WORDS);
    this.#data = new Uint8Array(buffer, USB_PROXY_RING_CTRL_BYTES);
    this.#cap = this.#data.byteLength >>> 0;
    this.#view = new DataView(this.#data.buffer, this.#data.byteOffset, this.#data.byteLength);
    if (this.#cap === 0) {
      throw new Error("USB proxy ring buffer is too small (missing data region).");
    }
  }

  buffer(): SharedArrayBuffer {
    return this.#ctrl.buffer as SharedArrayBuffer;
  }

  dataCapacityBytes(): number {
    return this.#cap;
  }

  dropped(): number {
    return u32(Atomics.load(this.#ctrl, CtrlIndex.Dropped));
  }

  pushAction(action: UsbHostAction): boolean {
    const kindTag = this.#actionKindToTag(action.kind);

    let recordSize = 0;
    let payloadLen = 0;

    switch (action.kind) {
      case "controlIn":
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES;
        break;
      case "controlOut":
        payloadLen = action.data.byteLength >>> 0;
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4 + payloadLen;
        break;
      case "bulkIn":
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + 8;
        break;
      case "bulkOut":
        payloadLen = action.data.byteLength >>> 0;
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + 8 + payloadLen;
        break;
      default: {
        const neverKind: never = action;
        void neverKind;
        Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
        return false;
      }
    }

    recordSize = alignUp(recordSize, USB_PROXY_RING_ALIGN);
    if (recordSize > this.#cap) {
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return false;
    }

    const { start, startIndex, reserve, tail } = this.#reserve(recordSize);
    if (startIndex === null) return false;

    // Header
    this.#view.setUint8(startIndex + 0, kindTag & 0xff);
    this.#view.setUint8(startIndex + 1, 0);
    this.#view.setUint16(startIndex + 2, 0, true);
    this.#view.setUint32(startIndex + 4, u32(action.id), true);

    switch (action.kind) {
      case "controlIn": {
        encodeSetupPacket(this.#view, startIndex + USB_PROXY_ACTION_HEADER_BYTES, action.setup);
        break;
      }
      case "controlOut": {
        encodeSetupPacket(this.#view, startIndex + USB_PROXY_ACTION_HEADER_BYTES, action.setup);
        this.#view.setUint32(startIndex + USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES, payloadLen >>> 0, true);
        const payloadStart = startIndex + USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4;
        this.#data.set(action.data, payloadStart);
        break;
      }
      case "bulkIn": {
        const base = startIndex + USB_PROXY_ACTION_HEADER_BYTES;
        this.#view.setUint8(base + 0, action.endpoint & 0xff);
        this.#view.setUint8(base + 1, 0);
        this.#view.setUint8(base + 2, 0);
        this.#view.setUint8(base + 3, 0);
        this.#view.setUint32(base + 4, u32(action.length), true);
        break;
      }
      case "bulkOut": {
        const base = startIndex + USB_PROXY_ACTION_HEADER_BYTES;
        this.#view.setUint8(base + 0, action.endpoint & 0xff);
        this.#view.setUint8(base + 1, 0);
        this.#view.setUint8(base + 2, 0);
        this.#view.setUint8(base + 3, 0);
        this.#view.setUint32(base + 4, payloadLen >>> 0, true);
        const payloadStart = startIndex + USB_PROXY_ACTION_HEADER_BYTES + 8;
        this.#data.set(action.data, payloadStart);
        break;
      }
    }

    const newTail = u32(tail + reserve);
    Atomics.store(this.#ctrl, CtrlIndex.Tail, newTail | 0);
    return true;
  }

  popAction(): UsbHostAction | null {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      if (head === tail) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;
      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) return null;

      const id = this.#view.getUint32(headIndex + 4, true) >>> 0;

      switch (kind) {
        case "controlIn": {
          const total = alignUp(USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES, USB_PROXY_RING_ALIGN);
          if (total > remaining) return null;
          const setup = decodeSetupPacket(this.#view, headIndex + USB_PROXY_ACTION_HEADER_BYTES);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind: "controlIn", id, setup };
        }
        case "controlOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4;
          if (fixed > remaining) return null;
          const setup = decodeSetupPacket(this.#view, base);
          const dataLen = this.#view.getUint32(base + SETUP_PACKET_BYTES, true) >>> 0;
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) return null;
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind: "controlOut", id, setup, data };
        }
        case "bulkIn": {
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
          if (total > remaining) return null;
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const endpoint = this.#view.getUint8(base + 0) >>> 0;
          const length = this.#view.getUint32(base + 4, true) >>> 0;
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind: "bulkIn", id, endpoint, length };
        }
        case "bulkOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          if (fixed > remaining) return null;
          const endpoint = this.#view.getUint8(base + 0) >>> 0;
          const dataLen = this.#view.getUint32(base + 4, true) >>> 0;
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) return null;
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind: "bulkOut", id, endpoint, data };
        }
      }
    }
  }

  pushCompletion(completion: UsbHostCompletion): boolean {
    const kindTag = this.#actionKindToTag(completion.kind);
    const statusTag = this.#completionStatusToTag(completion.status);

    let recordSize = USB_PROXY_COMPLETION_HEADER_BYTES;
    let payload: Uint8Array | null = null;

    if (completion.status === "success") {
      if (completion.kind === "controlIn" || completion.kind === "bulkIn") {
        payload = completion.data;
        recordSize += 4 + (payload.byteLength >>> 0);
      } else {
        recordSize += 4;
      }
    } else if (completion.status === "error") {
      payload = textEncoder.encode(completion.message);
      recordSize += 4 + payload.byteLength;
    }

    recordSize = alignUp(recordSize, USB_PROXY_RING_ALIGN);

    // Error messages are diagnostic only; truncate to fit rather than forcing
    // correctness-critical fallbacks.
    if (recordSize > this.#cap) {
      if (completion.status === "error") {
        const fixed = USB_PROXY_COMPLETION_HEADER_BYTES + 4;
        const maxTotal = this.#cap & ~(USB_PROXY_RING_ALIGN - 1);
        const maxPayload = maxTotal > fixed ? maxTotal - fixed : 0;
        payload = truncateUtf8(payload ?? new Uint8Array(), maxPayload);
        recordSize = alignUp(fixed + payload.byteLength, USB_PROXY_RING_ALIGN);
      }
    }

    if (recordSize > this.#cap) {
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return false;
    }

    const { startIndex, reserve, tail } = this.#reserve(recordSize);
    if (startIndex === null) return false;

    this.#view.setUint8(startIndex + 0, kindTag & 0xff);
    this.#view.setUint8(startIndex + 1, statusTag & 0xff);
    this.#view.setUint16(startIndex + 2, 0, true);
    this.#view.setUint32(startIndex + 4, u32(completion.id), true);

    const body = startIndex + USB_PROXY_COMPLETION_HEADER_BYTES;
    if (completion.status === "success") {
      if (completion.kind === "controlIn" || completion.kind === "bulkIn") {
        const len = payload ? (payload.byteLength >>> 0) : 0;
        this.#view.setUint32(body + 0, len, true);
        if (payload) this.#data.set(payload, body + 4);
      } else {
        this.#view.setUint32(body + 0, u32(completion.bytesWritten), true);
      }
    } else if (completion.status === "error") {
      const msgBytes = payload ?? new Uint8Array();
      this.#view.setUint32(body + 0, msgBytes.byteLength >>> 0, true);
      this.#data.set(msgBytes, body + 4);
    }

    const newTail = u32(tail + reserve);
    Atomics.store(this.#ctrl, CtrlIndex.Tail, newTail | 0);
    return true;
  }

  popCompletion(): UsbHostCompletion | null {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      if (head === tail) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;
      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) return null;

      const statusTag = this.#view.getUint8(headIndex + 1) as UsbCompletionStatusTag;
      const status = this.#completionTagToStatus(statusTag);
      if (!status) return null;

      const id = this.#view.getUint32(headIndex + 4, true) >>> 0;

      if (status === "stall") {
        const total = alignUp(USB_PROXY_COMPLETION_HEADER_BYTES, USB_PROXY_RING_ALIGN);
        if (total > remaining) return null;
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
        return { kind, id, status: "stall" };
      }

      const bodyIndex = headIndex + USB_PROXY_COMPLETION_HEADER_BYTES;
      const fixed = USB_PROXY_COMPLETION_HEADER_BYTES + 4;
      if (fixed > remaining) return null;

      if (status === "success") {
        if (kind === "controlIn" || kind === "bulkIn") {
          const dataLen = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) return null;
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, status: "success", data };
        }

        const bytesWritten = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
        const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
        if (total > remaining) return null;
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
        return { kind, id, status: "success", bytesWritten };
      }

      // status === "error"
      const msgLen = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
      const end = fixed + msgLen;
      const total = alignUp(end, USB_PROXY_RING_ALIGN);
      if (total > remaining) return null;
      const payloadStart = headIndex + fixed;
      const payloadEnd = payloadStart + msgLen;
      const msgBytes = this.#data.slice(payloadStart, payloadEnd);
      const message = textDecoder.decode(msgBytes);
      Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
      return { kind, id, status: "error", message };
    }
  }

  #reserve(recordSize: number): { start: number; startIndex: number | null; reserve: number; tail: number } {
    const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
    const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
    const used = u32(tail - head);
    if (used > this.#cap) {
      // Corruption or raced with a manual reset; treat as full.
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return { start: 0, startIndex: null, reserve: 0, tail };
    }
    const free = this.#cap - used;

    const tailIndex = tail % this.#cap;
    const remaining = this.#cap - tailIndex;
    const needsWrap = remaining >= USB_PROXY_RING_MIN_HEADER_BYTES && remaining < recordSize;
    const padding = remaining < recordSize ? remaining : 0;
    const reserve = padding + recordSize;
    if (reserve > free) {
      Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
      return { start: 0, startIndex: null, reserve: 0, tail };
    }

    if (needsWrap) {
      // For both action and completion rings the wrap marker is `kind=0xff` at the
      // current tail position. Consumers advance to the next wrap boundary.
      this.#view.setUint8(tailIndex + 0, UsbRecordKindTag.WrapMarker);
      this.#view.setUint8(tailIndex + 1, 0);
      this.#view.setUint16(tailIndex + 2, 0, true);
      this.#view.setUint32(tailIndex + 4, 0, true);
    }

    const start = u32(tail + padding);
    return { start, startIndex: start % this.#cap, reserve, tail };
  }

  #actionKindToTag(kind: UsbHostAction["kind"]): UsbRecordKindTag {
    switch (kind) {
      case "controlIn":
        return UsbRecordKindTag.ControlIn;
      case "controlOut":
        return UsbRecordKindTag.ControlOut;
      case "bulkIn":
        return UsbRecordKindTag.BulkIn;
      case "bulkOut":
        return UsbRecordKindTag.BulkOut;
      default: {
        const neverKind: never = kind;
        throw new Error(`Unknown UsbHostAction kind: ${String(neverKind)}`);
      }
    }
  }

  #actionTagToKind(tag: UsbRecordKindTag): UsbHostAction["kind"] | null {
    switch (tag) {
      case UsbRecordKindTag.ControlIn:
        return "controlIn";
      case UsbRecordKindTag.ControlOut:
        return "controlOut";
      case UsbRecordKindTag.BulkIn:
        return "bulkIn";
      case UsbRecordKindTag.BulkOut:
        return "bulkOut";
      default:
        return null;
    }
  }

  #completionStatusToTag(status: UsbHostCompletion["status"]): UsbCompletionStatusTag {
    switch (status) {
      case "success":
        return UsbCompletionStatusTag.Success;
      case "stall":
        return UsbCompletionStatusTag.Stall;
      case "error":
        return UsbCompletionStatusTag.Error;
      default: {
        const neverStatus: never = status;
        throw new Error(`Unknown UsbHostCompletion status: ${String(neverStatus)}`);
      }
    }
  }

  #completionTagToStatus(tag: UsbCompletionStatusTag): UsbHostCompletion["status"] | null {
    switch (tag) {
      case UsbCompletionStatusTag.Success:
        return "success";
      case UsbCompletionStatusTag.Stall:
        return "stall";
      case UsbCompletionStatusTag.Error:
        return "error";
      default:
        return null;
    }
  }
}
