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
  parseAeroIoSnapshotVersion,
  resolveVmSnapshotRestoreFromOpfsExport,
  resolveVmSnapshotSaveToOpfsExport,
  vmSnapshotDeviceIdToKind,
  vmSnapshotDeviceKindToId,
} from "./vm_snapshot_wasm";
import {
  decodeUsbSnapshotContainer,
  encodeUsbSnapshotContainer,
  USB_SNAPSHOT_TAG_EHCI,
  USB_SNAPSHOT_TAG_UHCI,
  USB_SNAPSHOT_TAG_XHCI,
} from "./usb_snapshot_container";

export type IoWorkerSnapshotDeviceState = { kind: string; bytes: Uint8Array };

export type IoWorkerSnapshotRuntimes = Readonly<{
  // Optional USB controller snapshot bridges/runtimes.
  //
  // All controller snapshots are stored under a single outer `DeviceId::USB` entry (kind
  // `usb.uhci` in the web runtime for historical reasons). When multiple controllers are present
  // (UHCI+EHCI+xHCI), we wrap their individual blobs in a deterministic container so they can be
  // snapshotted/restored together.
  usbXhciControllerBridge: unknown | null;
  usbUhciRuntime: unknown | null;
  usbUhciControllerBridge: unknown | null;
  usbEhciControllerBridge: unknown | null;
  i8042?: unknown | null;
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
  if (kind.startsWith(VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID)) {
    const id = vmSnapshotDeviceKindToId(kind);
    if (id !== null) return vmSnapshotDeviceIdToKind(id);
  }
  return kind;
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
      if (!tryLoadState(bridge, xhciBytes)) {
        console.warn(
          "[io.worker] Snapshot contains xHCI USB state but XhciControllerBridge has no load_state/restore_state hook; ignoring blob.",
        );
      }
    } catch (err) {
      console.warn("[io.worker] XhciControllerBridge load_state failed:", err);
    }
  };

  const restoreUhci = (uhciBytes: Uint8Array): void => {
    // Backwards compatibility: the entire blob is a UHCI snapshot.
    const runtime = runtimes.usbUhciRuntime;
    if (runtime) {
      try {
        if (tryLoadState(runtime, uhciBytes)) return;
      } catch (err) {
        console.warn("[io.worker] UhciRuntime load_state failed:", err);
      }
    }

    const bridge = runtimes.usbUhciControllerBridge;
    if (bridge) {
      try {
        tryLoadState(bridge, uhciBytes);
      } catch (err) {
        console.warn("[io.worker] UhciControllerBridge load_state failed:", err);
      }
    }
  };

  const decoded = decodeUsbSnapshotContainer(bytes);
  if (decoded) {
    const xhci = decoded.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_XHCI)?.bytes ?? null;
    const uhci = decoded.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_UHCI)?.bytes ?? null;
    const ehci = decoded.entries.find((e) => e.tag === USB_SNAPSHOT_TAG_EHCI)?.bytes ?? null;
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
    return;
  }

  // Legacy/raw USB snapshots: try xHCI first so older xHCI-only snapshots can be restored when an
  // xHCI bridge is present. If that fails, fall back to the historical UHCI restore path.
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

  // Merge in any previously restored device blobs so unknown/unhandled device state survives a
  // restore → save cycle (forward compatibility).
  const freshKinds = new Set(freshDevices.map((d) => d.kind));
  const devices: IoWorkerSnapshotDeviceState[] = [];
  const seen = new Set<string>();
  for (const cached of opts.restoredDevices ?? []) {
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
    let usbBytes: Uint8Array | null = null;
    let i8042Bytes: Uint8Array | null = null;
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

      let kind = normalizeRestoredDeviceKind(e.kind);
      if (kind === VM_SNAPSHOT_DEVICE_PCI_LEGACY_KIND && isPciBusSnapshot(e.bytes)) {
        kind = VM_SNAPSHOT_DEVICE_PCI_CFG_KIND;
      }
      devices.push({ kind, bytes: copyU8ToArrayBuffer(e.bytes) });
      restoredDevices.push({ kind, bytes: e.bytes });

      if (kind === VM_SNAPSHOT_DEVICE_USB_KIND) usbBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_I8042_KIND) i8042Bytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND) hdaBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND) virtioSndBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_PCI_CFG_KIND) pciBytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_E1000_KIND) e1000Bytes = e.bytes;
      if (kind === VM_SNAPSHOT_DEVICE_NET_STACK_KIND) stackBytes = e.bytes;
    }

    // Apply device state locally (IO worker owns USB + networking).
    if (usbBytes) restoreUsbDeviceState(opts.runtimes, usbBytes);
    if (i8042Bytes) restoreI8042DeviceState(opts.runtimes.i8042 ?? null, i8042Bytes);
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
