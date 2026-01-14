import type { WasmApi } from "../runtime/wasm_loader";
import type { VmSnapshotDeviceBlob } from "../runtime/snapshot_protocol";
import {
  VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND,
  VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND,
  VM_SNAPSHOT_DEVICE_E1000_KIND,
  VM_SNAPSHOT_DEVICE_I8042_KIND,
  VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID,
  VM_SNAPSHOT_DEVICE_NET_STACK_KIND,
  VM_SNAPSHOT_DEVICE_USB_KIND,
  VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND,
  parseAeroIoSnapshotVersion,
  resolveVmSnapshotRestoreFromOpfsExport,
  resolveVmSnapshotSaveToOpfsExport,
  vmSnapshotDeviceIdToKind,
  vmSnapshotDeviceKindToId,
} from "./vm_snapshot_wasm";
import {
  decodeUsbSnapshotContainer,
  encodeUsbSnapshotContainer,
  isUsbSnapshotContainer,
  USB_SNAPSHOT_TAG_EHCI,
  USB_SNAPSHOT_TAG_UHCI,
  USB_SNAPSHOT_TAG_XHCI,
} from "./usb_snapshot_container";

export type IoWorkerSnapshotDeviceState = { kind: string; bytes: Uint8Array };

export type IoWorkerSnapshotRuntimes = Readonly<{
  // Optional USB controller snapshot bridges/runtimes.
  //
  // The snapshot file format has a single outer `DeviceId::USB` entry (kind `usb`; legacy kind
  // `usb.uhci` is accepted for backwards compatibility).
  //
  // When multiple controllers exist (UHCI + optional EHCI/xHCI), their per-controller snapshots are
  // multiplexed into a single blob using the `"AUSB"` container format
  // (`web/src/workers/usb_snapshot_container.ts`), ensuring we always emit at most one USB device
  // state entry.
  usbXhciControllerBridge: unknown | null;
  usbUhciRuntime: unknown | null;
  usbUhciControllerBridge: unknown | null;
  usbEhciControllerBridge: unknown | null;
  i8042?: unknown | null;
  virtioInputKeyboard?: unknown | null;
  virtioInputMouse?: unknown | null;
  audioHda?: unknown | null;
  audioVirtioSnd?: unknown | null;
  pciBus?: unknown | null;
  netE1000: unknown | null;
  netStack: unknown | null;
}>;

// Snapshot kind strings for PCI config/bus state (web runtime).
//
// - Canonical: `aero_snapshot::DeviceId::PCI_CFG` (`14`)
// - Legacy/compat: some older web snapshots stored PCI config state under `DeviceId::PCI` (`5`)
//
// Note: We *only* treat legacy `device.5` blobs as PCI config state if they contain the expected
// inner `aero-io-snapshot` DEVICE_ID (`PCIB`). This avoids misinterpreting unrelated legacy
// `DeviceId::PCI` payloads (e.g. Rust `PCIC` / `PCPT` blobs) as the JS PCI bus snapshot.
const VM_SNAPSHOT_DEVICE_PCI_CFG_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}14`;
const VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}5`;
// `aero_snapshot::DeviceId::VIRTIO_INPUT` (24): virtio-input (virtio-pci) multi-function device
// wrapper (keyboard + mouse).
//
// This uses the canonical string kind (`input.virtio_input`) from `vm_snapshot_wasm.ts` so restore
// paths that normalize `device.<id>` kinds continue to work.
// Note: older snapshots may store this as `device.24`; `normalizeRestoredDeviceKind` maps that to
// the canonical `input.virtio_input` kind.

function isPciBusSnapshot(bytes: Uint8Array): boolean {
  // `web/src/io/bus/pci.ts` uses an `aero-io-snapshot`-shaped 16-byte header and sets
  // device_id="PCIB".
  return (
    bytes.byteLength >= 12 &&
    bytes[0] === 0x41 &&
    bytes[1] === 0x45 &&
    bytes[2] === 0x52 &&
    bytes[3] === 0x4f &&
    bytes[8] === 0x50 &&
    bytes[9] === 0x43 &&
    bytes[10] === 0x49 &&
    bytes[11] === 0x42
  );
}

function snapshotDeviceKindForWasm(kind: string): string {
  // WASM snapshot free-function exports historically understood `device.<id>` blobs (and may lag
  // behind the canonical string kinds). Prefer numeric IDs when possible so new device kinds can
  // roundtrip across older wasm builds.
  const id = vmSnapshotDeviceKindToId(kind);
  if (id === null) return kind;
  return `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}${id >>> 0}`;
}

function normalizeRestoredDeviceKind(kind: string): string {
  // Normalize:
  // - `device.<id>` → canonical kind string (or `device.<id>` for unknown IDs)
  // - legacy kind aliases (e.g. `usb.uhci`) → canonical kind string (`usb`)
  const id = vmSnapshotDeviceKindToId(kind);
  return id === null ? kind : vmSnapshotDeviceIdToKind(id);
}

function copyU8ToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out.buffer;
}

function trySaveState(instance: unknown): Uint8Array | null {
  if (!instance || typeof instance !== "object") return null;
  const save =
    (instance as unknown as { save_state?: unknown }).save_state ??
    (instance as unknown as { snapshot_state?: unknown }).snapshot_state ??
    (instance as unknown as { saveState?: unknown }).saveState ??
    (instance as unknown as { snapshotState?: unknown }).snapshotState;
  if (typeof save !== "function") return null;
  const bytes = save.call(instance) as unknown;
  return bytes instanceof Uint8Array ? bytes : null;
}

function tryLoadState(instance: unknown, bytes: Uint8Array): boolean {
  if (!instance || typeof instance !== "object") return false;
  const load =
    (instance as unknown as { load_state?: unknown }).load_state ??
    (instance as unknown as { restore_state?: unknown }).restore_state ??
    (instance as unknown as { loadState?: unknown }).loadState ??
    (instance as unknown as { restoreState?: unknown }).restoreState;
  if (typeof load !== "function") return false;
  load.call(instance, bytes);
  return true;
}

type UsbSnapshotKind = "xhci" | "uhci" | "ehci" | "unknown";

function readAeroIoDeviceIdTag(bytes: Uint8Array): string | null {
  // `aero-io-snapshot` TLV header:
  //   magic[4] = "AERO"
  //   ...
  //   device_id: [u8; 4] at offset 8
  if (
    bytes.byteLength >= 12 &&
    bytes[0] === 0x41 &&
    bytes[1] === 0x45 &&
    bytes[2] === 0x52 &&
    bytes[3] === 0x4f
  ) {
    const tag0 = bytes[8] ?? 0;
    const tag1 = bytes[9] ?? 0;
    const tag2 = bytes[10] ?? 0;
    const tag3 = bytes[11] ?? 0;
    const isAsciiTag = (b: number): boolean =>
      (b >= 0x30 && b <= 0x39) || // 0-9
      (b >= 0x41 && b <= 0x5a) || // A-Z
      (b >= 0x61 && b <= 0x7a) || // a-z
      b === 0x5f; // _
    if (isAsciiTag(tag0) && isAsciiTag(tag1) && isAsciiTag(tag2) && isAsciiTag(tag3)) {
      return String.fromCharCode(tag0, tag1, tag2, tag3);
    }
  }
  return null;
}

function classifyUsbSnapshot(bytes: Uint8Array): UsbSnapshotKind {
  const tag = readAeroIoDeviceIdTag(bytes);
  if (!tag) return "unknown";
  // Controller-specific tags:
  // - UHCI: `UHRT`, `UHCB`, `WUHB`, ...
  // - EHCI: `EHCI`, ...
  // - xHCI: `XHCI`, `XHCB`, ...
  if (tag.includes("XH")) return "xhci";
  if (tag.includes("UH")) return "uhci";
  if (tag.includes("EH")) return "ehci";
  return "unknown";
}

function isUsbSnapshotTagPrintable(tag: string): boolean {
  if (tag.length !== 4) return false;
  for (let i = 0; i < 4; i++) {
    const code = tag.charCodeAt(i);
    if (code < 0x20 || code > 0x7e) return false;
  }
  return true;
}

function usbSnapshotTagForLegacyBlob(bytes: Uint8Array): string {
  // Legacy snapshots stored a single raw controller blob. Most commonly this was UHCI, but
  // some historical builds also stored xHCI/EHCI snapshots directly under the outer
  // `DeviceId::USB` entry before the AUSB container was introduced.
  //
  // Classify the raw blob so restore→save merge semantics preserve the correct controller tag.
  const kind = classifyUsbSnapshot(bytes);
  if (kind === "xhci") return USB_SNAPSHOT_TAG_XHCI;
  if (kind === "ehci") return USB_SNAPSHOT_TAG_EHCI;
  return USB_SNAPSHOT_TAG_UHCI;
}

function mergeUsbSnapshotBytes(cached: Uint8Array, fresh: Uint8Array): Uint8Array {
  const cachedDecoded = decodeUsbSnapshotContainer(cached);
  const freshDecoded = decodeUsbSnapshotContainer(fresh);

  // If the "fresh" USB bytes look like an AUSB container but fail to decode, treat them as
  // corrupted and avoid attempting to merge.
  if (!freshDecoded && isUsbSnapshotContainer(fresh)) {
    return fresh;
  }

  // Interpret legacy bytes as a single-controller snapshot for backward compatibility.
  // (Most commonly UHCI, but some older builds stored raw xHCI/EHCI blobs under the outer USB entry.)
  // If the bytes *look* like a container but fail to decode, treat them as corrupt and drop them.
  const cachedEntries = cachedDecoded
    ? cachedDecoded.entries
    : isUsbSnapshotContainer(cached)
      ? []
      : [{ tag: usbSnapshotTagForLegacyBlob(cached), bytes: cached }];
  const freshEntries = freshDecoded ? freshDecoded.entries : [{ tag: usbSnapshotTagForLegacyBlob(fresh), bytes: fresh }];

  // Start with cached entries so unknown tags are preserved, then override with fresh entries so
  // newly snapshotted controllers take precedence.
  const merged = new Map<string, Uint8Array>();
  for (const e of cachedEntries) {
    if (!isUsbSnapshotTagPrintable(e.tag)) continue;
    if (!merged.has(e.tag)) merged.set(e.tag, e.bytes);
  }
  for (const e of freshEntries) {
    if (!isUsbSnapshotTagPrintable(e.tag)) continue;
    merged.set(e.tag, e.bytes);
  }

  if (merged.size === 0) {
    // Corrupt container (invalid/non-printable tags); fall back to the fresh snapshot bytes to
    // avoid hard-failing the overall save operation.
    return fresh;
  }

  const onlyUhci = merged.size === 1 && merged.has(USB_SNAPSHOT_TAG_UHCI);
  if (onlyUhci) {
    return merged.get(USB_SNAPSHOT_TAG_UHCI)!;
  }

  // Preserve container header metadata if present (forward compatibility).
  const version = freshDecoded?.version ?? cachedDecoded?.version;
  const flags = freshDecoded?.flags ?? cachedDecoded?.flags;
  const entries = Array.from(merged, ([tag, bytes]) => ({ tag, bytes }));

  return encodeUsbSnapshotContainer(entries, { version, flags });
}

// -------------------------------------------------------------------------------------------------
// virtio-input snapshot wrapper (VINP)
// -------------------------------------------------------------------------------------------------

// `aero_machine::MachineVirtioInputSnapshot` encodes virtio-input keyboard+mouse state as an
// `aero-io-snapshot` TLV wrapper:
// - magic: "AERO"
// - device_id: "VINP"
// - device_version: 1.0
// - TAG 1: nested keyboard virtio-pci snapshot bytes
// - TAG 2: nested mouse virtio-pci snapshot bytes
const VIRTIO_INPUT_SNAPSHOT_TAG_KEYBOARD = 1;
const VIRTIO_INPUT_SNAPSHOT_TAG_MOUSE = 2;

type VirtioInputSnapshotContainer = {
  version: number;
  flags: number;
  entries: Array<{ tag: number; bytes: Uint8Array }>;
};

function isVirtioInputSnapshotContainer(bytes: Uint8Array): boolean {
  return (
    bytes.byteLength >= 12 &&
    bytes[0] === 0x41 && // A
    bytes[1] === 0x45 && // E
    bytes[2] === 0x52 && // R
    bytes[3] === 0x4f && // O
    bytes[8] === 0x56 && // V
    bytes[9] === 0x49 && // I
    bytes[10] === 0x4e && // N
    bytes[11] === 0x50 // P
  );
}

function decodeVirtioInputSnapshotContainer(bytes: Uint8Array): VirtioInputSnapshotContainer | null {
  if (!isVirtioInputSnapshotContainer(bytes)) return null;
  if (bytes.byteLength < 16) return null;

  const version = (bytes[12]! | (bytes[13]! << 8)) >>> 0;
  const flags = (bytes[14]! | (bytes[15]! << 8)) >>> 0;

  const entries: Array<{ tag: number; bytes: Uint8Array }> = [];
  const seen = new Set<number>();
  let off = 16;
  while (off < bytes.byteLength) {
    if (off + 6 > bytes.byteLength) return null;
    const tag = (bytes[off]! | (bytes[off + 1]! << 8)) >>> 0;
    const len =
      (bytes[off + 2]! | (bytes[off + 3]! << 8) | (bytes[off + 4]! << 16) | (bytes[off + 5]! << 24)) >>> 0;
    off += 6;
    const end = off + len;
    if (!Number.isSafeInteger(end) || end < off || end > bytes.byteLength) return null;
    if (seen.has(tag)) return null;
    entries.push({ tag, bytes: bytes.subarray(off, end) });
    seen.add(tag);
    off = end;
  }

  return { version, flags, entries };
}

function encodeVirtioInputSnapshotContainer(
  entries: Array<{ tag: number; bytes: Uint8Array }>,
  opts?: { version?: number; flags?: number },
): Uint8Array {
  const version = opts?.version ?? 1;
  const flags = opts?.flags ?? 0;

  const sorted = [...entries].sort((a, b) => a.tag - b.tag);
  let total = 16;
  for (const e of sorted) total += 6 + e.bytes.byteLength;

  const out = new Uint8Array(total);
  // magic "AERO"
  out[0] = 0x41;
  out[1] = 0x45;
  out[2] = 0x52;
  out[3] = 0x4f;
  // format version 1.0
  out[4] = 0x01;
  out[5] = 0x00;
  out[6] = 0x00;
  out[7] = 0x00;
  // device id "VINP"
  out[8] = 0x56;
  out[9] = 0x49;
  out[10] = 0x4e;
  out[11] = 0x50;
  // device version (major/minor)
  out[12] = version & 0xff;
  out[13] = (version >>> 8) & 0xff;
  out[14] = flags & 0xff;
  out[15] = (flags >>> 8) & 0xff;

  let off = 16;
  for (const e of sorted) {
    const tag = e.tag >>> 0;
    const len = e.bytes.byteLength >>> 0;
    out[off] = tag & 0xff;
    out[off + 1] = (tag >>> 8) & 0xff;
    out[off + 2] = len & 0xff;
    out[off + 3] = (len >>> 8) & 0xff;
    out[off + 4] = (len >>> 16) & 0xff;
    out[off + 5] = (len >>> 24) & 0xff;
    off += 6;
    out.set(e.bytes, off);
    off += len;
  }

  return out;
}

function mergeVirtioInputSnapshotBytes(cached: Uint8Array, fresh: Uint8Array): Uint8Array {
  const cachedDecoded = decodeVirtioInputSnapshotContainer(cached);
  const freshDecoded = decodeVirtioInputSnapshotContainer(fresh);

  const cachedLooksLikeContainer = !cachedDecoded && isVirtioInputSnapshotContainer(cached);
  const freshLooksLikeContainer = !freshDecoded && isVirtioInputSnapshotContainer(fresh);
  if (freshLooksLikeContainer) {
    console.warn("[io.worker] virtio-input snapshot appears to be a VINP container but failed to decode; skipping merge.");
    return fresh;
  }
  if (!freshDecoded) return fresh;

  const cachedEntries = cachedDecoded
    ? cachedDecoded.entries
    : cachedLooksLikeContainer
      ? []
      : [];
  const freshEntries = freshDecoded.entries;

  // Preserve unknown tags from cached, then override with fresh tags so newly snapshotted
  // virtio-input functions take precedence.
  const merged = new Map<number, Uint8Array>();
  for (const e of cachedEntries) {
    if (!merged.has(e.tag)) merged.set(e.tag, e.bytes);
  }
  for (const e of freshEntries) {
    merged.set(e.tag, e.bytes);
  }
  if (merged.size === 0) return fresh;

  const version = freshDecoded.version ?? cachedDecoded?.version;
  const flags = freshDecoded.flags ?? cachedDecoded?.flags;
  const entries = Array.from(merged, ([tag, bytes]) => ({ tag, bytes }));
  return encodeVirtioInputSnapshotContainer(entries, { version, flags });
}

export function snapshotUsbDeviceState(
  runtimes: Pick<
    IoWorkerSnapshotRuntimes,
    "usbXhciControllerBridge" | "usbUhciRuntime" | "usbUhciControllerBridge" | "usbEhciControllerBridge"
  >,
): IoWorkerSnapshotDeviceState | null {
  let xhciBytes: Uint8Array | null = null;
  const xhci = runtimes.usbXhciControllerBridge;
  if (xhci) {
    try {
      const bytes = trySaveState(xhci);
      if (bytes) xhciBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] XhciControllerBridge save_state failed:", err);
    }
  }

  let uhciBytes: Uint8Array | null = null;
  const runtime = runtimes.usbUhciRuntime;
  if (runtime) {
    try {
      const bytes = trySaveState(runtime);
      if (bytes) uhciBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] UhciRuntime save_state failed:", err);
    }
  }

  const bridge = runtimes.usbUhciControllerBridge;
  if (!uhciBytes && bridge) {
    try {
      const bytes = trySaveState(bridge);
      if (bytes) uhciBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] UhciControllerBridge save_state failed:", err);
    }
  }

  let ehciBytes: Uint8Array | null = null;
  const ehci = runtimes.usbEhciControllerBridge;
  if (ehci) {
    try {
      const bytes = trySaveState(ehci);
      if (bytes) ehciBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] EhciControllerBridge save_state failed:", err);
    }
  }

  if (!xhciBytes && !uhciBytes && !ehciBytes) return null;

  // Backwards compatibility: older snapshots contain a single UHCI blob.
  if (uhciBytes && !xhciBytes && !ehciBytes) return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes: uhciBytes };

  const entries: Array<{ tag: string; bytes: Uint8Array }> = [];
  if (xhciBytes) entries.push({ tag: USB_SNAPSHOT_TAG_XHCI, bytes: xhciBytes });
  if (uhciBytes) entries.push({ tag: USB_SNAPSHOT_TAG_UHCI, bytes: uhciBytes });
  if (ehciBytes) entries.push({ tag: USB_SNAPSHOT_TAG_EHCI, bytes: ehciBytes });
  return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes: encodeUsbSnapshotContainer(entries) };
}

export function snapshotI8042DeviceState(i8042: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!i8042) return null;
  try {
    const bytes = trySaveState(i8042);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_I8042_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] input.i8042 save_state failed:", err);
  }
  return null;
}

export function snapshotVirtioInputDeviceState(
  runtimes: Pick<IoWorkerSnapshotRuntimes, "virtioInputKeyboard" | "virtioInputMouse">,
): IoWorkerSnapshotDeviceState | null {
  let keyboardBytes: Uint8Array | null = null;
  const keyboard = runtimes.virtioInputKeyboard ?? null;
  if (keyboard) {
    try {
      const bytes = trySaveState(keyboard);
      if (bytes) keyboardBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] virtio-input keyboard save_state failed:", err);
    }
  }

  let mouseBytes: Uint8Array | null = null;
  const mouse = runtimes.virtioInputMouse ?? null;
  if (mouse) {
    try {
      const bytes = trySaveState(mouse);
      if (bytes) mouseBytes = bytes;
    } catch (err) {
      console.warn("[io.worker] virtio-input mouse save_state failed:", err);
    }
  }

  if (!keyboardBytes && !mouseBytes) return null;

  const entries: Array<{ tag: number; bytes: Uint8Array }> = [];
  if (keyboardBytes) entries.push({ tag: VIRTIO_INPUT_SNAPSHOT_TAG_KEYBOARD, bytes: keyboardBytes });
  if (mouseBytes) entries.push({ tag: VIRTIO_INPUT_SNAPSHOT_TAG_MOUSE, bytes: mouseBytes });
  return { kind: VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND, bytes: encodeVirtioInputSnapshotContainer(entries) };
}

export function snapshotNetE1000DeviceState(netE1000: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!netE1000) return null;
  try {
    const bytes = trySaveState(netE1000);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_E1000_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] net.e1000 save_state failed:", err);
  }
  return null;
}

export function snapshotAudioHdaDeviceState(audioHda: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!audioHda) return null;
  try {
    const bytes = trySaveState(audioHda);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] audio.hda save_state failed:", err);
  }
  return null;
}

export function snapshotAudioVirtioSndDeviceState(audioVirtioSnd: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!audioVirtioSnd) return null;
  try {
    const bytes = trySaveState(audioVirtioSnd);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] audio.virtio_snd save_state failed:", err);
  }
  return null;
}

export function snapshotPciDeviceState(pciBus: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!pciBus) return null;
  try {
    const bytes = trySaveState(pciBus);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_PCI_CFG_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] PCI saveState failed:", err);
  }
  return null;
}

export function snapshotNetStackDeviceState(netStack: unknown | null): IoWorkerSnapshotDeviceState | null {
  if (!netStack) return null;
  try {
    const bytes = trySaveState(netStack);
    if (bytes) return { kind: VM_SNAPSHOT_DEVICE_NET_STACK_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] net.stack save_state failed:", err);
  }
  return null;
}

export function collectIoWorkerSnapshotDeviceStates(runtimes: IoWorkerSnapshotRuntimes): IoWorkerSnapshotDeviceState[] {
  const devices: IoWorkerSnapshotDeviceState[] = [];

  const usb = snapshotUsbDeviceState(runtimes);
  if (usb) devices.push(usb);

  const i8042 = snapshotI8042DeviceState(runtimes.i8042 ?? null);
  if (i8042) devices.push(i8042);

  const virtioInput = snapshotVirtioInputDeviceState(runtimes);
  if (virtioInput) devices.push(virtioInput);

  const hda = snapshotAudioHdaDeviceState(runtimes.audioHda ?? null);
  if (hda) devices.push(hda);

  const virtioSnd = snapshotAudioVirtioSndDeviceState(runtimes.audioVirtioSnd ?? null);
  if (virtioSnd) devices.push(virtioSnd);

  const pci = snapshotPciDeviceState(runtimes.pciBus ?? null);
  if (pci) devices.push(pci);

  const e1000 = snapshotNetE1000DeviceState(runtimes.netE1000);
  if (e1000) devices.push(e1000);

  const stack = snapshotNetStackDeviceState(runtimes.netStack);
  if (stack) devices.push(stack);

  return devices;
}

export function restoreUsbDeviceState(
  runtimes: Pick<
    IoWorkerSnapshotRuntimes,
    "usbXhciControllerBridge" | "usbUhciRuntime" | "usbUhciControllerBridge" | "usbEhciControllerBridge"
  >,
  bytes: Uint8Array,
): void {
  const restoreXhci = (xhciBytes: Uint8Array): void => {
    const bridge = runtimes.usbXhciControllerBridge;
    if (!bridge) {
      console.warn("[io.worker] Snapshot contains xHCI USB state but XhciControllerBridge runtime is unavailable; ignoring blob.");
      return;
    }

    try {
      if (tryLoadState(bridge, xhciBytes)) return;
      console.warn("[io.worker] Snapshot contains xHCI USB state but XhciControllerBridge has no load_state/restore_state hook; ignoring blob.");
    } catch (err) {
      console.warn("[io.worker] XhciControllerBridge load_state failed:", err);
    }
  };

  const restoreUhci = (uhciBytes: Uint8Array): void => {
    // Backwards compatibility: the entire blob is a UHCI snapshot.
    const runtime = runtimes.usbUhciRuntime;
    const bridge = runtimes.usbUhciControllerBridge;

    if (!runtime && !bridge) {
      console.warn("[io.worker] Snapshot contains UHCI USB state but UHCI runtime/bridge is unavailable; ignoring blob.");
      return;
    }

    if (runtime) {
      try {
        const ok = tryLoadState(runtime, uhciBytes);
        if (ok) return;
      } catch (err) {
        console.warn("[io.worker] UhciRuntime load_state failed:", err);
        if (!bridge) return;
      }
    }

    if (bridge) {
      try {
        if (tryLoadState(bridge, uhciBytes)) return;
        console.warn(
          "[io.worker] Snapshot contains UHCI USB state but UhciControllerBridge has no load_state/restore_state hook; ignoring blob.",
        );
        return;
      } catch (err) {
        console.warn("[io.worker] UhciControllerBridge load_state failed:", err);
        return;
      }
    }

    // Runtime exists but did not accept restore (no hook).
    console.warn("[io.worker] Snapshot contains UHCI USB state but UhciRuntime has no load_state/restore_state hook; ignoring blob.");
  };

  const decoded = decodeUsbSnapshotContainer(bytes);
  if (decoded) {
    // If a container has duplicate controller tags, apply a deterministic last-wins policy.
    // (The encoder always emits unique tags, but be defensive when restoring snapshots produced
    // by other builds.)
    const byTag = new Map<string, Uint8Array>();
    for (const e of decoded.entries) byTag.set(e.tag, e.bytes);

    const xhci = byTag.get(USB_SNAPSHOT_TAG_XHCI) ?? null;
    const uhci = byTag.get(USB_SNAPSHOT_TAG_UHCI) ?? null;
    const ehci = byTag.get(USB_SNAPSHOT_TAG_EHCI) ?? null;
    if (xhci) restoreXhci(xhci);
    if (uhci) restoreUhci(uhci);
    if (ehci) {
      const bridge = runtimes.usbEhciControllerBridge;
      if (!bridge) {
        console.warn("[io.worker] Snapshot contains EHCI USB state but EhciControllerBridge runtime is unavailable; ignoring blob.");
      } else {
        try {
          if (!tryLoadState(bridge, ehci)) {
            console.warn(
              "[io.worker] Snapshot contains EHCI USB state but EhciControllerBridge has no load_state/restore_state hook; ignoring blob.",
            );
          }
        } catch (err) {
          console.warn("[io.worker] EhciControllerBridge load_state failed:", err);
        }
      }
    }

    // Forward compatibility: warn on unknown controller tags so debugging missing controller
    // support is straightforward.
    const warnedUnknown = new Set<string>();
    for (const tag of byTag.keys()) {
      if (tag === USB_SNAPSHOT_TAG_XHCI || tag === USB_SNAPSHOT_TAG_UHCI || tag === USB_SNAPSHOT_TAG_EHCI) continue;
      if (warnedUnknown.has(tag)) continue;
      warnedUnknown.add(tag);
      console.warn(`[io.worker] Snapshot contains unknown USB controller tag ${JSON.stringify(tag)}; ignoring blob.`);
    }
    return;
  }

  if (isUsbSnapshotContainer(bytes)) {
    console.warn("[io.worker] Snapshot USB blob has AUSB container magic but is corrupt; ignoring blob.");
    return;
  }

  const kind = classifyUsbSnapshot(bytes);

  if (kind === "xhci") {
    // xHCI snapshots should never be applied to UHCI; if xHCI is unavailable, ignore with warning.
    restoreXhci(bytes);
    return;
  }

  if (kind === "ehci") {
    const bridge = runtimes.usbEhciControllerBridge;
    if (!bridge) {
      console.warn("[io.worker] Snapshot contains EHCI USB state but EhciControllerBridge runtime is unavailable; ignoring blob.");
      return;
    }
    try {
      if (!tryLoadState(bridge, bytes)) {
        console.warn("[io.worker] Snapshot contains EHCI USB state but EhciControllerBridge has no load_state/restore_state hook; ignoring blob.");
      }
    } catch (err) {
      console.warn("[io.worker] EhciControllerBridge load_state failed:", err);
    }
    return;
  }

  if (kind === "uhci") {
    restoreUhci(bytes);
    return;
  }

  // For unknown/legacy blobs, preserve older behavior: if xHCI exists and accepts the blob, use it;
  // otherwise fall back to UHCI.
  const xhciBridge = runtimes.usbXhciControllerBridge;
  if (xhciBridge) {
    try {
      if (tryLoadState(xhciBridge, bytes)) return;
    } catch (err) {
      console.warn("[io.worker] XhciControllerBridge load_state failed:", err);
    }
  }

  restoreUhci(bytes);
}

export function restoreI8042DeviceState(i8042: unknown | null, bytes: Uint8Array): void {
  if (!i8042) {
    console.warn("[io.worker] Snapshot contains input.i8042 state but i8042 runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(i8042, bytes)) {
      console.warn(
        "[io.worker] Snapshot contains input.i8042 state but i8042 runtime has no load_state/restore_state hook; ignoring blob.",
      );
    }
  } catch (err) {
    console.warn("[io.worker] input.i8042 load_state failed:", err);
  }
}

export function restoreVirtioInputKeyboardDeviceState(virtioInputKeyboard: unknown | null, bytes: Uint8Array): void {
  if (!virtioInputKeyboard) {
    console.warn("[io.worker] Snapshot contains input.virtio_keyboard state but virtio-input keyboard runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(virtioInputKeyboard, bytes)) {
      console.warn(
        "[io.worker] Snapshot contains input.virtio_keyboard state but virtio-input keyboard runtime has no load_state/restore_state hook; ignoring blob.",
      );
    }
  } catch (err) {
    console.warn("[io.worker] input.virtio_keyboard load_state failed:", err);
  }
}

export function restoreVirtioInputMouseDeviceState(virtioInputMouse: unknown | null, bytes: Uint8Array): void {
  if (!virtioInputMouse) {
    console.warn("[io.worker] Snapshot contains input.virtio_mouse state but virtio-input mouse runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(virtioInputMouse, bytes)) {
      console.warn(
        "[io.worker] Snapshot contains input.virtio_mouse state but virtio-input mouse runtime has no load_state/restore_state hook; ignoring blob.",
      );
    }
  } catch (err) {
    console.warn("[io.worker] input.virtio_mouse load_state failed:", err);
  }
}

export function restoreVirtioInputDeviceState(
  runtimes: Pick<IoWorkerSnapshotRuntimes, "virtioInputKeyboard" | "virtioInputMouse">,
  bytes: Uint8Array,
): void {
  const decoded = decodeVirtioInputSnapshotContainer(bytes);
  if (!decoded) {
    if (isVirtioInputSnapshotContainer(bytes)) {
      console.warn("[io.worker] Snapshot virtio-input blob has VINP container magic but is corrupt; ignoring blob.");
    } else {
      console.warn("[io.worker] Snapshot contains virtio-input state but blob is not a VINP container; ignoring blob.");
    }
    return;
  }

  const keyboard =
    decoded.entries.find((e) => e.tag === VIRTIO_INPUT_SNAPSHOT_TAG_KEYBOARD)?.bytes ?? null;
  const mouse =
    decoded.entries.find((e) => e.tag === VIRTIO_INPUT_SNAPSHOT_TAG_MOUSE)?.bytes ?? null;
  if (keyboard) {
    restoreVirtioInputKeyboardDeviceState(runtimes.virtioInputKeyboard ?? null, keyboard);
  }
  if (mouse) {
    restoreVirtioInputMouseDeviceState(runtimes.virtioInputMouse ?? null, mouse);
  }
}

export function restoreNetE1000DeviceState(netE1000: unknown | null, bytes: Uint8Array): void {
  if (!netE1000) {
    console.warn("[io.worker] Snapshot contains net.e1000 state but networking runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(netE1000, bytes)) {
      console.warn("[io.worker] Snapshot contains net.e1000 state but net.e1000 runtime has no load_state/restore_state hook; ignoring blob.");
    }
  } catch (err) {
    console.warn("[io.worker] net.e1000 load_state failed:", err);
  }
}

export function restoreAudioHdaDeviceState(audioHda: unknown | null, bytes: Uint8Array): void {
  if (!audioHda) {
    console.warn("[io.worker] Snapshot contains audio.hda state but audio runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(audioHda, bytes)) {
      console.warn("[io.worker] Snapshot contains audio.hda state but audio runtime has no load_state/restore_state hook; ignoring blob.");
    }
  } catch (err) {
    console.warn("[io.worker] audio.hda load_state failed:", err);
  }
}

export function restoreAudioVirtioSndDeviceState(audioVirtioSnd: unknown | null, bytes: Uint8Array): void {
  if (!audioVirtioSnd) {
    console.warn("[io.worker] Snapshot contains audio.virtio_snd state but audio runtime is unavailable; ignoring blob.");
    return;
  }
  try {
    if (!tryLoadState(audioVirtioSnd, bytes)) {
      console.warn(
        "[io.worker] Snapshot contains audio.virtio_snd state but audio runtime has no load_state/restore_state hook; ignoring blob.",
      );
    }
  } catch (err) {
    console.warn("[io.worker] audio.virtio_snd load_state failed:", err);
  }
}

export function restorePciDeviceState(pciBus: unknown | null, bytes: Uint8Array): void {
  if (!pciBus) return;
  try {
    if (!tryLoadState(pciBus, bytes)) {
      console.warn("[io.worker] Snapshot contains PCI state but PCI runtime has no load_state/restore_state hook; ignoring blob.");
    }
  } catch (err) {
    console.warn("[io.worker] PCI loadState failed:", err);
  }
}

function applyNetStackTcpRestorePolicy(netStack: unknown, policy: "drop" | "reconnect"): void {
  if (!netStack || (typeof netStack !== "object" && typeof netStack !== "function")) return;

  const fn =
    (netStack as unknown as { apply_tcp_restore_policy?: unknown }).apply_tcp_restore_policy ??
    (netStack as unknown as { applyTcpRestorePolicy?: unknown }).applyTcpRestorePolicy ??
    (netStack as unknown as { set_tcp_restore_policy?: unknown }).set_tcp_restore_policy ??
    (netStack as unknown as { setTcpRestorePolicy?: unknown }).setTcpRestorePolicy;
  if (typeof fn !== "function") return;

  // Prefer a readable string API, but fall back to a numeric enum-like encoding if
  // the runtime expects it (e.g. wasm-bindgen enum variants).
  try {
    fn.call(netStack, policy);
    return;
  } catch {
    // continue
  }
  try {
    fn.call(netStack, policy === "drop" ? 0 : 1);
  } catch (err) {
    console.warn("[io.worker] net.stack apply_tcp_restore_policy failed:", err);
  }
}

export function restoreNetStackDeviceState(netStack: unknown | null, bytes: Uint8Array, opts?: { tcpRestorePolicy?: "drop" | "reconnect" }): void {
  if (!netStack) {
    console.warn("[io.worker] Snapshot contains net.stack state but networking runtime is unavailable; ignoring blob.");
    return;
  }

  try {
    if (!tryLoadState(netStack, bytes)) {
      console.warn("[io.worker] Snapshot contains net.stack state but net.stack runtime has no load_state/restore_state hook; ignoring blob.");
      return;
    }
  } catch (err) {
    console.warn("[io.worker] net.stack load_state failed:", err);
    return;
  }

  applyNetStackTcpRestorePolicy(netStack, opts?.tcpRestorePolicy ?? "drop");
}

export async function saveIoWorkerVmSnapshotToOpfs(opts: {
  api: WasmApi;
  path: string;
  cpu: ArrayBuffer;
  mmu: ArrayBuffer;
  guestBase: number;
  guestSize: number;
  runtimes: IoWorkerSnapshotRuntimes;
  /**
   * Device blobs recovered from the most recent VM snapshot restore. These are merged into the
   * next save so unknown/unhandled device state survives a restore → save cycle (forward
   * compatibility).
   */
  restoredDevices?: IoWorkerSnapshotDeviceState[];
  /**
   * Optional device blobs supplied by the coordinator (typically CPU-owned device state such as
   * CPU_INTERNAL).
   */
  coordinatorDevices?: VmSnapshotDeviceBlob[];
}): Promise<void> {
  // Normalize any device kind aliases in the cached/restored device list.
  //
  // In the common case, `restoredDevices` comes from `restoreIoWorkerVmSnapshotFromOpfs` and is
  // already canonicalized (e.g. legacy `usb.uhci` gets mapped to `usb`). However, be defensive in
  // case callers pass through raw snapshot device kinds.
  const restoredDevices: IoWorkerSnapshotDeviceState[] = (opts.restoredDevices ?? []).map((d) => ({
    kind: normalizeRestoredDeviceKind(d.kind),
    bytes: d.bytes,
  }));

  // Fresh device blobs produced by this IO worker.
  const freshDevices: IoWorkerSnapshotDeviceState[] = collectIoWorkerSnapshotDeviceStates(opts.runtimes);

  // Merge in any coordinator-provided device blobs (e.g. CPU-owned device state like CPU_INTERNAL).
  // Treat these as "fresh" so they override any cached restored blobs of the same kind.
  if (Array.isArray(opts.coordinatorDevices)) {
    for (const dev of opts.coordinatorDevices) {
      if (!dev || typeof dev !== "object") continue;
      if (typeof (dev as { kind?: unknown }).kind !== "string") continue;
      if (!((dev as { bytes?: unknown }).bytes instanceof ArrayBuffer)) continue;
      const kind = normalizeRestoredDeviceKind((dev as { kind: string }).kind);
      freshDevices.push({ kind, bytes: new Uint8Array((dev as { bytes: ArrayBuffer }).bytes) });
    }
  }

  // USB snapshots are a special case: multiple controller snapshots (UHCI/EHCI/xHCI/...) are
  // multiplexed into a single `DeviceId::USB` (`usb`) device blob via `usb_snapshot_container.ts`.
  //
  // If we previously restored a USB container that includes controllers not present in the current
  // build (e.g. snapshot taken on a newer build with EHCI/xHCI), preserve those controller blobs
  // across a restore → save cycle by merging container entries.
  const cachedUsb = restoredDevices.find((d) => d.kind === VM_SNAPSHOT_DEVICE_USB_KIND) ?? null;
  const freshUsbIndex = freshDevices.findIndex((d) => d.kind === VM_SNAPSHOT_DEVICE_USB_KIND);
  if (cachedUsb && freshUsbIndex >= 0) {
    const freshUsb = freshDevices[freshUsbIndex]!;
    try {
      freshDevices[freshUsbIndex] = { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes: mergeUsbSnapshotBytes(cachedUsb.bytes, freshUsb.bytes) };
    } catch (err) {
      console.warn("[io.worker] Failed to merge cached USB snapshot container entries; using fresh USB snapshot only.", err);
    }
  }

  // virtio-input snapshots are a multi-function wrapper (`VINP`) containing nested per-function
  // virtio-pci snapshots. Preserve unknown/new wrapper tags across restore → save cycles by
  // merging wrapper entries when both cached+fresh blobs are present.
  const cachedVirtioInput = restoredDevices.find((d) => d.kind === VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND) ?? null;
  const freshVirtioInputIndex = freshDevices.findIndex((d) => d.kind === VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND);
  if (cachedVirtioInput && freshVirtioInputIndex >= 0) {
    const freshVirtioInput = freshDevices[freshVirtioInputIndex]!;
    try {
      freshDevices[freshVirtioInputIndex] = {
        kind: VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND,
        bytes: mergeVirtioInputSnapshotBytes(cachedVirtioInput.bytes, freshVirtioInput.bytes),
      };
    } catch (err) {
      console.warn("[io.worker] Failed to merge cached virtio-input snapshot container entries; using fresh virtio-input snapshot only.", err);
    }
  }

  // Merge in any previously restored device blobs so unknown/unhandled device state survives a
  // restore → save cycle (forward compatibility).
  const freshKinds = new Set(freshDevices.map((d) => d.kind));
  const devices: IoWorkerSnapshotDeviceState[] = [];
  const seen = new Set<string>();
  for (const cached of restoredDevices) {
    if (freshKinds.has(cached.kind)) continue;
    if (seen.has(cached.kind)) continue;
    devices.push(cached);
    seen.add(cached.kind);
  }
  for (const dev of freshDevices) {
    if (seen.has(dev.kind)) continue;
    devices.push(dev);
    seen.add(dev.kind);
  }

  const saveExport = resolveVmSnapshotSaveToOpfsExport(opts.api);
  if (!saveExport) {
    throw new Error("WASM VM snapshot save export is unavailable (expected *_snapshot*_to_opfs or WorkerVmSnapshot).");
  }

  if (saveExport.kind === "free-function") {
    // Build a JS-friendly device blob list; wasm-bindgen can accept this as `JsValue`.
    const devicePayload = devices.map((d) => ({ kind: snapshotDeviceKindForWasm(d.kind), bytes: d.bytes }));

    // Always pass fresh Uint8Array views for the CPU state so callers can transfer the ArrayBuffer.
    const cpuBytes = new Uint8Array(opts.cpu);
    const mmuBytes = new Uint8Array(opts.mmu);

    await Promise.resolve(saveExport.fn.call(opts.api as unknown, opts.path, cpuBytes, mmuBytes, devicePayload));
    return;
  }

  const builder = new saveExport.Ctor(opts.guestBase >>> 0, opts.guestSize >>> 0);
  try {
    builder.set_cpu_state_v2(new Uint8Array(opts.cpu), new Uint8Array(opts.mmu));

    for (const device of devices) {
      const id = vmSnapshotDeviceKindToId(device.kind);
      if (id === null) {
        throw new Error(`Unsupported VM snapshot device kind: ${device.kind}`);
      }
      // CPU_INTERNAL (`DeviceId::CPU_INTERNAL = 9`) uses a raw v2 encoding (no `AERO` header), so
      // we must not rely on `parseAeroIoSnapshotVersion`'s default fallback (v1.0).
      const { version, flags } = id === 9 ? { version: 2, flags: 0 } : parseAeroIoSnapshotVersion(device.bytes);
      builder.add_device_state(id, version, flags, device.bytes);
    }

    await builder.snapshot_full_to_opfs(opts.path);
  } finally {
    try {
      builder.free();
    } catch {
      // ignore
    }
  }
}

export async function restoreIoWorkerVmSnapshotFromOpfs(opts: {
  api: WasmApi;
  path: string;
  guestBase: number;
  guestSize: number;
  runtimes: IoWorkerSnapshotRuntimes;
}): Promise<{
  cpu: ArrayBuffer;
  mmu: ArrayBuffer;
  devices?: VmSnapshotDeviceBlob[];
  /**
   * Normalized device blobs recovered from the snapshot file, suitable for caching and merging
   * into subsequent saves.
   */
  restoredDevices: IoWorkerSnapshotDeviceState[];
}> {
  const restoreExport = resolveVmSnapshotRestoreFromOpfsExport(opts.api);
  if (!restoreExport) {
    throw new Error("WASM VM snapshot restore export is unavailable (expected *_restore*_from_opfs or WorkerVmSnapshot).");
  }

  if (restoreExport.kind === "free-function") {
    const res = await Promise.resolve(restoreExport.fn.call(opts.api as unknown, opts.path));
    const rec = res as { cpu?: unknown; mmu?: unknown; devices?: unknown };
    if (!(rec?.cpu instanceof Uint8Array) || !(rec?.mmu instanceof Uint8Array)) {
      throw new Error("WASM snapshot restore returned an unexpected result shape (expected {cpu:Uint8Array, mmu:Uint8Array}).");
    }

    const devicesRaw = Array.isArray(rec.devices) ? rec.devices : [];
    const devices: VmSnapshotDeviceBlob[] = [];
    const restoredDevices: IoWorkerSnapshotDeviceState[] = [];
    // When multiple USB blobs are present (shouldn't happen), prefer the canonical kind over legacy
    // aliases so restores are deterministic.
    let usbBytes: Uint8Array | null = null;
    let usbPriority = -1;
    let i8042Bytes: Uint8Array | null = null;
    let virtioInputBytes: Uint8Array | null = null;
    let hdaBytes: Uint8Array | null = null;
    let virtioSndBytes: Uint8Array | null = null;
    let pciBytes: Uint8Array | null = null;
    let e1000Bytes: Uint8Array | null = null;
    let stackBytes: Uint8Array | null = null;
    for (const entry of devicesRaw) {
      if (!entry || typeof entry !== "object") continue;
      const e = entry as { kind?: unknown; bytes?: unknown };
      if (typeof e.kind !== "string") continue;
      if (!(e.bytes instanceof Uint8Array)) continue;

      const rawKind = e.kind;
      let kind = normalizeRestoredDeviceKind(rawKind);
      if (kind === VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND && isPciBusSnapshot(e.bytes)) {
        kind = VM_SNAPSHOT_DEVICE_PCI_CFG_KIND;
      }
      devices.push({ kind, bytes: copyU8ToArrayBuffer(e.bytes) });
      restoredDevices.push({ kind, bytes: e.bytes });

      if (kind === VM_SNAPSHOT_DEVICE_USB_KIND) {
        // Canonical USB kind should win if present.
        const nextPriority =
          rawKind === VM_SNAPSHOT_DEVICE_USB_KIND
            ? 2
            : rawKind.startsWith(VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID)
              ? 1
              : 0;
        if (nextPriority > usbPriority) {
          usbPriority = nextPriority;
          usbBytes = e.bytes;
        }
      }
      if (kind === VM_SNAPSHOT_DEVICE_I8042_KIND) i8042Bytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND) virtioInputBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND) hdaBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND) virtioSndBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_PCI_CFG_KIND) pciBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_E1000_KIND) e1000Bytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_NET_STACK_KIND) stackBytes = e.bytes;
    }

    // Apply device state locally (IO worker owns USB + networking).
    if (usbBytes) restoreUsbDeviceState(opts.runtimes, usbBytes);
    if (i8042Bytes) restoreI8042DeviceState(opts.runtimes.i8042 ?? null, i8042Bytes);
    if (virtioInputBytes) restoreVirtioInputDeviceState(opts.runtimes, virtioInputBytes);
    if (hdaBytes) restoreAudioHdaDeviceState(opts.runtimes.audioHda ?? null, hdaBytes);
    if (virtioSndBytes) restoreAudioVirtioSndDeviceState(opts.runtimes.audioVirtioSnd ?? null, virtioSndBytes);
    if (e1000Bytes) restoreNetE1000DeviceState(opts.runtimes.netE1000, e1000Bytes);
    if (pciBytes) restorePciDeviceState(opts.runtimes.pciBus ?? null, pciBytes);
    if (stackBytes) restoreNetStackDeviceState(opts.runtimes.netStack, stackBytes, { tcpRestorePolicy: "drop" });

    return {
      cpu: copyU8ToArrayBuffer(rec.cpu),
      mmu: copyU8ToArrayBuffer(rec.mmu),
      devices: devices.length ? devices : undefined,
      restoredDevices,
    };
  }

  const builder = new restoreExport.Ctor(opts.guestBase >>> 0, opts.guestSize >>> 0);
  try {
    const res = await builder.restore_snapshot_from_opfs(opts.path);
    const rec = res as { cpu?: unknown; mmu?: unknown; devices?: unknown };
    if (!(rec?.cpu instanceof Uint8Array) || !(rec?.mmu instanceof Uint8Array) || !Array.isArray(rec.devices)) {
      throw new Error("WASM snapshot restore returned an unexpected result shape (expected {cpu:Uint8Array, mmu:Uint8Array, devices:Array}).");
    }

    const devices: VmSnapshotDeviceBlob[] = [];
    const restoredDevices: IoWorkerSnapshotDeviceState[] = [];
    let usbBytes: Uint8Array | null = null;
    let i8042Bytes: Uint8Array | null = null;
    let virtioInputBytes: Uint8Array | null = null;
    let hdaBytes: Uint8Array | null = null;
    let virtioSndBytes: Uint8Array | null = null;
    let pciBytes: Uint8Array | null = null;
    let e1000Bytes: Uint8Array | null = null;
    let stackBytes: Uint8Array | null = null;
    for (const entry of rec.devices) {
      if (!entry || typeof entry !== "object") {
        throw new Error("WASM snapshot restore returned an unexpected devices entry (expected {id:number,version:number,flags:number,data:Uint8Array}).");
      }
      const e = entry as { id?: unknown; version?: unknown; flags?: unknown; data?: unknown };
      if (typeof e.id !== "number" || typeof e.version !== "number" || typeof e.flags !== "number" || !(e.data instanceof Uint8Array)) {
        throw new Error("WASM snapshot restore returned an unexpected devices entry shape (expected {id:number,version:number,flags:number,data:Uint8Array}).");
      }

      const kind = vmSnapshotDeviceIdToKind(e.id);
      if (!kind) continue;
      const kindIsLegacyPci = kind === VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND && isPciBusSnapshot(e.data);
      const canonicalKind = kindIsLegacyPci ? VM_SNAPSHOT_DEVICE_PCI_CFG_KIND : kind;

      if (canonicalKind === VM_SNAPSHOT_DEVICE_USB_KIND) usbBytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_I8042_KIND) i8042Bytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_VIRTIO_INPUT_KIND) virtioInputBytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND) hdaBytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND) virtioSndBytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_PCI_CFG_KIND) pciBytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_E1000_KIND) e1000Bytes = e.data;
      if (canonicalKind === VM_SNAPSHOT_DEVICE_NET_STACK_KIND) stackBytes = e.data;

      restoredDevices.push({ kind: canonicalKind, bytes: e.data });
      devices.push({ kind: canonicalKind, bytes: copyU8ToArrayBuffer(e.data) });
    }

    if (usbBytes) restoreUsbDeviceState(opts.runtimes, usbBytes);
    if (i8042Bytes) restoreI8042DeviceState(opts.runtimes.i8042 ?? null, i8042Bytes);
    if (virtioInputBytes) restoreVirtioInputDeviceState(opts.runtimes, virtioInputBytes);
    if (hdaBytes) restoreAudioHdaDeviceState(opts.runtimes.audioHda ?? null, hdaBytes);
    if (virtioSndBytes) restoreAudioVirtioSndDeviceState(opts.runtimes.audioVirtioSnd ?? null, virtioSndBytes);
    if (e1000Bytes) restoreNetE1000DeviceState(opts.runtimes.netE1000, e1000Bytes);
    if (pciBytes) restorePciDeviceState(opts.runtimes.pciBus ?? null, pciBytes);
    if (stackBytes) restoreNetStackDeviceState(opts.runtimes.netStack, stackBytes, { tcpRestorePolicy: "drop" });

    return {
      cpu: copyU8ToArrayBuffer(rec.cpu),
      mmu: copyU8ToArrayBuffer(rec.mmu),
      devices: devices.length ? devices : undefined,
      restoredDevices,
    };
  } finally {
    try {
      builder.free();
    } catch {
      // ignore
    }
  }
}
