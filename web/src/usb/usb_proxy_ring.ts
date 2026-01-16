import type { SetupPacket, UsbHostAction, UsbHostCompletion } from "./usb_passthrough_types";
import { MAX_USB_PROXY_BYTES, type UsbProxyActionOptions } from "./usb_proxy_protocol";
import { DEFAULT_ERROR_BYTE_LIMITS } from "../errors/serialize";
import { formatOneLineUtf8 } from "../text";

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
//   flags: u8
//   reserved: u16
//   id: u32 (LE)
export const USB_PROXY_ACTION_HEADER_BYTES = 8;

// Action header flags (byte 1).
const USB_PROXY_ACTION_FLAG_DISABLE_OTHER_SPEED_CONFIG_TRANSLATION = 1 << 0;

// Completion records share an 8-byte header:
//   kind: u8
//   status: u8
//   reserved: u16
//   id: u32 (LE)
export const USB_PROXY_COMPLETION_HEADER_BYTES = 8;

const CtrlIndex = {
  Head: 0,
  Tail: 1,
  Dropped: 2,
} as const;

const UsbRecordKindTag = {
  ControlIn: 1,
  ControlOut: 2,
  BulkIn: 3,
  BulkOut: 4,
  WrapMarker: 0xff,
} as const;

const UsbCompletionStatusTag = {
  Success: 0,
  Stall: 1,
  Error: 2,
} as const;

type UsbRecordKindTag = (typeof UsbRecordKindTag)[keyof typeof UsbRecordKindTag];
type UsbCompletionStatusTag = (typeof UsbCompletionStatusTag)[keyof typeof UsbCompletionStatusTag];

const SETUP_PACKET_BYTES = 8;

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function u32(n: number): number {
  return n >>> 0;
}

function alignUp(value: number, align: number): number {
  if (align <= 0 || (align & (align - 1)) !== 0) throw new Error("align must be a positive power of two");
  const rem = value % align;
  return rem === 0 ? value : value + (align - rem);
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

// Wire cap for diagnostic messages sent through the ring. Encoding/decoding additionally clamps
// messages to `DEFAULT_ERROR_BYTE_LIMITS` so oversized/multi-line strings never escape the ring.
const MAX_USB_PROXY_ERROR_MESSAGE_BYTES = 16 * 1024;

function encodeUtf8TruncatedString(input: string, maxBytes: number): Uint8Array {
  if (maxBytes <= 0) return new Uint8Array();

  // Avoid allocating a giant temporary `Uint8Array` when `input` is huge by using
  // `TextEncoder.encodeInto` with a bounded destination buffer.
  const buf = new Uint8Array(maxBytes);
  const { read, written } = textEncoder.encodeInto(input, buf);

  // Fast path: the string fully fit in `maxBytes`.
  if (read === input.length) return buf.subarray(0, written);

  if (maxBytes <= TRUNCATION_MARKER_BYTES.byteLength) return TRUNCATION_MARKER_BYTES.slice(0, maxBytes);

  // Truncate and append a marker so callers can distinguish "truncated due to limit" vs
  // "message naturally ended here". Preserve `truncateUtf8` semantics: cap by UTF-8 bytes while
  // avoiding invalid trailing partial code points.
  const headMax = maxBytes - TRUNCATION_MARKER_BYTES.byteLength;
  const headLen = Math.min(headMax, written);
  const out = new Uint8Array(headLen + TRUNCATION_MARKER_BYTES.byteLength);
  out.set(buf.subarray(0, headLen), 0);
  out.set(TRUNCATION_MARKER_BYTES, headLen);
  return out;
}

function isUsbEndpointAddress(value: number): boolean {
  // A USB endpoint address is a u8 with:
  // - bit7: direction (IN=1, OUT=0)
  // - bits4..6: reserved (must be 0)
  // - bits0..3: endpoint number (1..=15 for non-control endpoints)
  const u = value >>> 0;
  if (u !== value || u > 0xff) return false;
  return (u & 0x70) === 0 && (u & 0x0f) !== 0;
}

function isUsbInEndpointAddress(value: number): boolean {
  return isUsbEndpointAddress(value) && (value & 0x80) !== 0;
}

function isUsbOutEndpointAddress(value: number): boolean {
  return isUsbEndpointAddress(value) && (value & 0x80) === 0;
}

export function createUsbProxyRingBuffer(dataCapacityBytes: number): SharedArrayBuffer {
  if (!Number.isSafeInteger(dataCapacityBytes) || dataCapacityBytes <= 0) {
    throw new Error(`dataCapacityBytes must be a positive safe integer (got ${String(dataCapacityBytes)})`);
  }
  // Avoid accidental multi-gigabyte allocations; this is plenty for USB proxy traffic. Larger transfers
  // can fall back to structured postMessage forwarding.
  const max = 16 * 1024 * 1024;
  if (dataCapacityBytes > max) {
    throw new Error(`dataCapacityBytes must be <= ${max} (got ${String(dataCapacityBytes)})`);
  }
  // Record parsing relies on 4-byte alignment.
  const cap = alignUp(dataCapacityBytes >>> 0, USB_PROXY_RING_ALIGN);
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

  /**
   * Peek the next action record's payload byte length without mutating the ring.
   *
   * This is intended for *action rings* (worker -> main thread) so the broker can
   * apply backpressure before popping/allocating large payload buffers.
   *
   * Returns:
   * - `null` when the ring is empty (or only contains wrap padding)
   * - `0` for non-payload actions (`controlIn`, `bulkIn`)
   * - `>0` for payload actions (`controlOut`, `bulkOut`)
   *
   * Throws when ring corruption is detected.
   */
  peekNextActionPayloadBytes(): number | null {
    const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
    let head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
    let used = u32(tail - head);
    if (used > this.#cap) {
      throw new Error("USB proxy ring corrupted (action ring tail/head out of range).");
    }
    if (used === 0) return null;

    // Simulate the consumer's wrap/marker skipping logic without mutating `head`.
    while (used !== 0) {
      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;

      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring head/tail inconsistent: incomplete wrap padding).");
        }
        head = u32(head + remaining);
        used = u32(used - remaining);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring wrap marker beyond tail).");
        }
        head = u32(head + remaining);
        used = u32(used - remaining);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) {
        throw new Error(`USB proxy ring corrupted (unknown action kind tag: ${kindTag}).`);
      }

      switch (kind) {
        case "controlIn": {
          const total = alignUp(USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlIn record exceeds available bytes).");
          return 0;
        }
        case "bulkIn": {
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkIn record exceeds available bytes).");
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const length = this.#view.getUint32(base + 4, true) >>> 0;
          if (length > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkIn length too large: ${length} bytes).`);
          }
          return 0;
        }
        case "controlOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (controlOut record straddles wrap boundary).");
          const setup = decodeSetupPacket(this.#view, base);
          const dataLen = this.#view.getUint32(base + SETUP_PACKET_BYTES, true) >>> 0;
          if (dataLen !== setup.wLength) {
            throw new Error(
              `USB proxy ring corrupted (controlOut payload length mismatch: wLength=${setup.wLength} dataLen=${dataLen}).`,
            );
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlOut record exceeds available bytes).");
          return dataLen;
        }
        case "bulkOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (bulkOut record straddles wrap boundary).");
          const dataLen = this.#view.getUint32(base + 4, true) >>> 0;
          if (dataLen > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkOut payload too large: ${dataLen} bytes).`);
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkOut record exceeds available bytes).");
          return dataLen;
        }
      }
    }

    return null;
  }

  pushAction(action: UsbHostAction, options?: UsbProxyActionOptions): boolean {
    // Producer-side validation: keep the ring robust by refusing to encode records that the
    // consumer would later treat as corruption (and detach the SAB fast-path).
    let recordSize = 0;
    let payloadLen = 0;

    switch (action.kind) {
      case "controlIn":
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES;
        break;
      case "controlOut":
        payloadLen = action.data.byteLength >>> 0;
        if (payloadLen > MAX_USB_PROXY_BYTES) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        if (payloadLen !== (action.setup.wLength & 0xffff)) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4 + payloadLen;
        break;
      case "bulkIn":
        if (!isUsbInEndpointAddress(action.endpoint)) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        if ((action.length >>> 0) > MAX_USB_PROXY_BYTES) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        recordSize = USB_PROXY_ACTION_HEADER_BYTES + 8;
        break;
      case "bulkOut":
        payloadLen = action.data.byteLength >>> 0;
        if (!isUsbOutEndpointAddress(action.endpoint)) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        if (payloadLen > MAX_USB_PROXY_BYTES) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
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

    const kindTag = this.#actionKindToTag(action.kind);
    const flags =
      options?.translateOtherSpeedConfigurationDescriptor === false
        ? USB_PROXY_ACTION_FLAG_DISABLE_OTHER_SPEED_CONFIG_TRANSLATION
        : 0;

    // Header
    this.#view.setUint8(startIndex + 0, kindTag & 0xff);
    this.#view.setUint8(startIndex + 1, flags & 0xff);
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

  popActionRecord(): { action: UsbHostAction; options?: UsbProxyActionOptions } | null {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      const used = u32(tail - head);
      if (used > this.#cap) {
        throw new Error("USB proxy ring corrupted (action ring tail/head out of range).");
      }
      if (used === 0) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;
      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring head/tail inconsistent: incomplete wrap padding).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring head/tail inconsistent: wrap marker beyond tail).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) {
        throw new Error(`USB proxy ring corrupted (unknown action kind tag: ${kindTag}).`);
      }

      const flags = this.#view.getUint8(headIndex + 1) >>> 0;
      const id = this.#view.getUint32(headIndex + 4, true) >>> 0;

      const options: UsbProxyActionOptions | undefined =
        (flags & USB_PROXY_ACTION_FLAG_DISABLE_OTHER_SPEED_CONFIG_TRANSLATION) !== 0
          ? { translateOtherSpeedConfigurationDescriptor: false }
          : undefined;

      switch (kind) {
        case "controlIn": {
          const total = alignUp(USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlIn record exceeds available bytes).");
          const setup = decodeSetupPacket(this.#view, headIndex + USB_PROXY_ACTION_HEADER_BYTES);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { action: { kind: "controlIn", id, setup }, options };
        }
        case "controlOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (controlOut record straddles wrap boundary).");
          const setup = decodeSetupPacket(this.#view, base);
          const dataLen = this.#view.getUint32(base + SETUP_PACKET_BYTES, true) >>> 0;
          if (dataLen !== setup.wLength) {
            throw new Error(
              `USB proxy ring corrupted (controlOut payload length mismatch: wLength=${setup.wLength} dataLen=${dataLen}).`,
            );
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlOut record exceeds available bytes).");
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { action: { kind: "controlOut", id, setup, data }, options };
        }
        case "bulkIn": {
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkIn record exceeds available bytes).");
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const endpoint = this.#view.getUint8(base + 0) >>> 0;
          const length = this.#view.getUint32(base + 4, true) >>> 0;
          if (length > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkIn length too large: ${length} bytes).`);
          }
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { action: { kind: "bulkIn", id, endpoint, length }, options };
        }
        case "bulkOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (bulkOut record straddles wrap boundary).");
          const endpoint = this.#view.getUint8(base + 0) >>> 0;
          const dataLen = this.#view.getUint32(base + 4, true) >>> 0;
          if (dataLen > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkOut payload too large: ${dataLen} bytes).`);
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkOut record exceeds available bytes).");
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { action: { kind: "bulkOut", id, endpoint, data }, options };
        }
      }
    }
  }

  /**
   * Pop the next action record without copying payload bytes out of the ring.
   *
   * This is useful for error paths in the main-thread broker: when no WebUSB device is selected, we still
   * want to drain pending actions and return error completions, but copying large `bulkOut`/`controlOut`
   * buffers out of the SharedArrayBuffer would be wasted work.
   *
   * Returns `null` when the ring is empty.
   *
   * Throws when ring corruption is detected.
   */
  popActionInfo(): { kind: UsbHostAction["kind"]; id: number; options?: UsbProxyActionOptions; payloadBytes: number } | null {
    // eslint-disable-next-line no-constant-condition
    while (true) {
      const head = u32(Atomics.load(this.#ctrl, CtrlIndex.Head));
      const tail = u32(Atomics.load(this.#ctrl, CtrlIndex.Tail));
      const used = u32(tail - head);
      if (used > this.#cap) {
        throw new Error("USB proxy ring corrupted (action ring tail/head out of range).");
      }
      if (used === 0) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;
      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring head/tail inconsistent: incomplete wrap padding).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (action ring head/tail inconsistent: wrap marker beyond tail).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) {
        throw new Error(`USB proxy ring corrupted (unknown action kind tag: ${kindTag}).`);
      }

      const flags = this.#view.getUint8(headIndex + 1) >>> 0;
      const id = this.#view.getUint32(headIndex + 4, true) >>> 0;

      const options: UsbProxyActionOptions | undefined =
        (flags & USB_PROXY_ACTION_FLAG_DISABLE_OTHER_SPEED_CONFIG_TRANSLATION) !== 0
          ? { translateOtherSpeedConfigurationDescriptor: false }
          : undefined;

      switch (kind) {
        case "controlIn": {
          const total = alignUp(USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlIn record exceeds available bytes).");
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, options, payloadBytes: 0 };
        }
        case "controlOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + SETUP_PACKET_BYTES + 4;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (controlOut record straddles wrap boundary).");
          const setup = decodeSetupPacket(this.#view, base);
          const dataLen = this.#view.getUint32(base + SETUP_PACKET_BYTES, true) >>> 0;
          if (dataLen !== setup.wLength) {
            throw new Error(
              `USB proxy ring corrupted (controlOut payload length mismatch: wLength=${setup.wLength} dataLen=${dataLen}).`,
            );
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (controlOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (controlOut record exceeds available bytes).");
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, options, payloadBytes: dataLen };
        }
        case "bulkIn": {
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkIn record straddles wrap boundary).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkIn record exceeds available bytes).");
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const length = this.#view.getUint32(base + 4, true) >>> 0;
          if (length > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkIn length too large: ${length} bytes).`);
          }
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, options, payloadBytes: 0 };
        }
        case "bulkOut": {
          const base = headIndex + USB_PROXY_ACTION_HEADER_BYTES;
          const fixed = USB_PROXY_ACTION_HEADER_BYTES + 8;
          if (fixed > remaining) throw new Error("USB proxy ring corrupted (bulkOut record straddles wrap boundary).");
          const dataLen = this.#view.getUint32(base + 4, true) >>> 0;
          if (dataLen > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (bulkOut payload too large: ${dataLen} bytes).`);
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (bulkOut payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (bulkOut record exceeds available bytes).");
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, options, payloadBytes: dataLen };
        }
      }
    }
  }

  popAction(): UsbHostAction | null {
    const record = this.popActionRecord();
    return record ? record.action : null;
  }

  pushCompletion(completion: UsbHostCompletion): boolean {
    const kindTag = this.#actionKindToTag(completion.kind);
    const statusTag = this.#completionStatusToTag(completion.status);

    let recordSize = USB_PROXY_COMPLETION_HEADER_BYTES;
    let payload: Uint8Array | null = null;

    if (completion.status === "success") {
      if (completion.kind === "controlIn" || completion.kind === "bulkIn") {
        payload = completion.data;
        if ((payload.byteLength >>> 0) > MAX_USB_PROXY_BYTES) {
          Atomics.add(this.#ctrl, CtrlIndex.Dropped, 1);
          return false;
        }
        recordSize += 4 + (payload.byteLength >>> 0);
      } else {
        recordSize += 4;
      }
    } else if (completion.status === "error") {
      const fixed = USB_PROXY_COMPLETION_HEADER_BYTES + 4;
      const ringMax = this.#cap > fixed ? this.#cap - fixed : 0;
      const maxBytes = Math.min(ringMax, MAX_USB_PROXY_ERROR_MESSAGE_BYTES);
      const safeMessage = formatOneLineUtf8(completion.message, DEFAULT_ERROR_BYTE_LIMITS.maxMessageBytes) || "Error";
      payload = encodeUtf8TruncatedString(safeMessage, maxBytes);
      recordSize += 4 + payload.byteLength;
    }

    recordSize = alignUp(recordSize, USB_PROXY_RING_ALIGN);

    // Error messages are diagnostic only; truncate to fit (see `encodeUtf8TruncatedString`) rather
    // than forcing correctness-critical fallbacks.

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
      const used = u32(tail - head);
      if (used > this.#cap) {
        throw new Error("USB proxy ring corrupted (completion ring tail/head out of range).");
      }
      if (used === 0) return null;

      const headIndex = head % this.#cap;
      const remaining = this.#cap - headIndex;
      if (remaining < USB_PROXY_RING_MIN_HEADER_BYTES) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (completion ring head/tail inconsistent: incomplete wrap padding).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kindTag = this.#view.getUint8(headIndex + 0) as UsbRecordKindTag;
      if (kindTag === UsbRecordKindTag.WrapMarker) {
        if (remaining > used) {
          throw new Error("USB proxy ring corrupted (completion ring head/tail inconsistent: wrap marker beyond tail).");
        }
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + remaining) | 0);
        continue;
      }

      const kind = this.#actionTagToKind(kindTag);
      if (!kind) {
        throw new Error(`USB proxy ring corrupted (unknown completion kind tag: ${kindTag}).`);
      }

      const statusTag = this.#view.getUint8(headIndex + 1) as UsbCompletionStatusTag;
      const status = this.#completionTagToStatus(statusTag);
      if (!status) {
        throw new Error(`USB proxy ring corrupted (unknown completion status tag: ${statusTag}).`);
      }

      const id = this.#view.getUint32(headIndex + 4, true) >>> 0;

      if (status === "stall") {
        const total = alignUp(USB_PROXY_COMPLETION_HEADER_BYTES, USB_PROXY_RING_ALIGN);
        if (total > remaining) throw new Error("USB proxy ring corrupted (stall record straddles wrap boundary).");
        if (total > used) throw new Error("USB proxy ring corrupted (stall record exceeds available bytes).");
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
        return { kind, id, status: "stall" };
      }

      const bodyIndex = headIndex + USB_PROXY_COMPLETION_HEADER_BYTES;
      const fixed = USB_PROXY_COMPLETION_HEADER_BYTES + 4;
      if (fixed > remaining) throw new Error("USB proxy ring corrupted (completion header straddles wrap boundary).");
      if (fixed > used) throw new Error("USB proxy ring corrupted (completion record exceeds available bytes).");

      if (status === "success") {
        if (kind === "controlIn" || kind === "bulkIn") {
          const dataLen = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
          if (dataLen > MAX_USB_PROXY_BYTES) {
            throw new Error(`USB proxy ring corrupted (completion payload too large: ${dataLen} bytes).`);
          }
          const end = fixed + dataLen;
          const total = alignUp(end, USB_PROXY_RING_ALIGN);
          if (total > remaining) throw new Error("USB proxy ring corrupted (completion payload exceeds ring segment).");
          if (total > used) throw new Error("USB proxy ring corrupted (completion payload exceeds available bytes).");
          const payloadStart = headIndex + fixed;
          const payloadEnd = payloadStart + dataLen;
          const data = this.#data.slice(payloadStart, payloadEnd);
          Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
          return { kind, id, status: "success", data };
        }

        const bytesWritten = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
        const total = alignUp(fixed, USB_PROXY_RING_ALIGN);
        if (total > remaining) throw new Error("USB proxy ring corrupted (completion record straddles wrap boundary).");
        if (total > used) throw new Error("USB proxy ring corrupted (completion record exceeds available bytes).");
        Atomics.store(this.#ctrl, CtrlIndex.Head, u32(head + total) | 0);
        return { kind, id, status: "success", bytesWritten };
      }

      // status === "error"
      const msgLen = this.#view.getUint32(bodyIndex + 0, true) >>> 0;
      if (msgLen > MAX_USB_PROXY_ERROR_MESSAGE_BYTES) {
        throw new Error(`USB proxy ring corrupted (error message too large: ${msgLen} bytes).`);
      }
      const end = fixed + msgLen;
      const total = alignUp(end, USB_PROXY_RING_ALIGN);
      if (total > remaining) throw new Error("USB proxy ring corrupted (error payload exceeds ring segment).");
      if (total > used) throw new Error("USB proxy ring corrupted (error payload exceeds available bytes).");
      const payloadStart = headIndex + fixed;
      const payloadEnd = payloadStart + msgLen;
      const msgBytes = this.#data.slice(payloadStart, payloadEnd);
      const rawMessage = textDecoder.decode(msgBytes);
      const message = formatOneLineUtf8(rawMessage, DEFAULT_ERROR_BYTE_LIMITS.maxMessageBytes) || "Error";
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
