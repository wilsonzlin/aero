import type { WasmApi } from "../runtime/wasm_loader";
import type { VmSnapshotDeviceBlob } from "../runtime/snapshot_protocol";
import {
  VM_SNAPSHOT_DEVICE_E1000_KIND,
  VM_SNAPSHOT_DEVICE_NET_STACK_KIND,
  VM_SNAPSHOT_DEVICE_USB_KIND,
  parseAeroIoSnapshotVersion,
  resolveVmSnapshotRestoreFromOpfsExport,
  resolveVmSnapshotSaveToOpfsExport,
  vmSnapshotDeviceIdToKind,
  vmSnapshotDeviceKindToId,
} from "./vm_snapshot_wasm";

export type IoWorkerSnapshotDeviceState = { kind: string; bytes: Uint8Array };

export type IoWorkerSnapshotRuntimes = Readonly<{
  usbUhciRuntime: unknown | null;
  usbUhciControllerBridge: unknown | null;
  netE1000: unknown | null;
  netStack: unknown | null;
}>;

function copyU8ToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out.buffer;
}

function trySaveState(instance: unknown): Uint8Array | null {
  if (!instance || typeof instance !== "object") return null;
  const save =
    (instance as unknown as { save_state?: unknown }).save_state ??
    (instance as unknown as { snapshot_state?: unknown }).snapshot_state;
  if (typeof save !== "function") return null;
  const bytes = save.call(instance) as unknown;
  return bytes instanceof Uint8Array ? bytes : null;
}

function tryLoadState(instance: unknown, bytes: Uint8Array): boolean {
  if (!instance || typeof instance !== "object") return false;
  const load =
    (instance as unknown as { load_state?: unknown }).load_state ??
    (instance as unknown as { restore_state?: unknown }).restore_state;
  if (typeof load !== "function") return false;
  load.call(instance, bytes);
  return true;
}

export function snapshotUsbDeviceState(runtimes: Pick<IoWorkerSnapshotRuntimes, "usbUhciRuntime" | "usbUhciControllerBridge">): IoWorkerSnapshotDeviceState | null {
  const runtime = runtimes.usbUhciRuntime;
  if (runtime) {
    try {
      const bytes = trySaveState(runtime);
      if (bytes) return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes };
    } catch (err) {
      console.warn("[io.worker] UhciRuntime save_state failed:", err);
    }
  }

  const bridge = runtimes.usbUhciControllerBridge;
  if (bridge) {
    try {
      const bytes = trySaveState(bridge);
      if (bytes) return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes };
    } catch (err) {
      console.warn("[io.worker] UhciControllerBridge save_state failed:", err);
    }
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

  const e1000 = snapshotNetE1000DeviceState(runtimes.netE1000);
  if (e1000) devices.push(e1000);

  const stack = snapshotNetStackDeviceState(runtimes.netStack);
  if (stack) devices.push(stack);

  return devices;
}

export function restoreUsbDeviceState(runtimes: Pick<IoWorkerSnapshotRuntimes, "usbUhciRuntime" | "usbUhciControllerBridge">, bytes: Uint8Array): void {
  const runtime = runtimes.usbUhciRuntime;
  if (runtime) {
    try {
      if (tryLoadState(runtime, bytes)) return;
    } catch (err) {
      console.warn("[io.worker] UhciRuntime load_state failed:", err);
    }
  }

  const bridge = runtimes.usbUhciControllerBridge;
  if (bridge) {
    try {
      tryLoadState(bridge, bytes);
    } catch (err) {
      console.warn("[io.worker] UhciControllerBridge load_state failed:", err);
    }
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

function applyNetStackTcpRestorePolicy(netStack: unknown, policy: "drop" | "reconnect"): void {
  if (!netStack || typeof netStack !== "object") return;

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
}): Promise<void> {
  const devices = collectIoWorkerSnapshotDeviceStates(opts.runtimes);

  const saveExport = resolveVmSnapshotSaveToOpfsExport(opts.api);
  if (!saveExport) {
    throw new Error("WASM VM snapshot save export is unavailable (expected *_snapshot*_to_opfs or WorkerVmSnapshot).");
  }

  if (saveExport.kind === "free-function") {
    // Build a JS-friendly device blob list; wasm-bindgen can accept this as `JsValue`.
    const devicePayload = devices.map((d) => ({ kind: d.kind, bytes: d.bytes }));

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
      const { version, flags } = parseAeroIoSnapshotVersion(device.bytes);
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
    for (const entry of devicesRaw) {
      if (!entry || typeof entry !== "object") continue;
      const e = entry as { kind?: unknown; bytes?: unknown };
      if (typeof e.kind !== "string") continue;
      if (!(e.bytes instanceof Uint8Array)) continue;
      devices.push({ kind: e.kind, bytes: copyU8ToArrayBuffer(e.bytes) });
    }

    // Apply device state locally (IO worker owns USB + networking).
    const usbBlob = devicesRaw.find(
      (entry): entry is { kind: string; bytes: Uint8Array } =>
        !!entry &&
        typeof (entry as { kind?: unknown }).kind === "string" &&
        (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_USB_KIND &&
        (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
    );
    if (usbBlob) restoreUsbDeviceState(opts.runtimes, usbBlob.bytes);

    const e1000Blob = devicesRaw.find(
      (entry): entry is { kind: string; bytes: Uint8Array } =>
        !!entry &&
        typeof (entry as { kind?: unknown }).kind === "string" &&
        (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_E1000_KIND &&
        (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
    );
    if (e1000Blob) restoreNetE1000DeviceState(opts.runtimes.netE1000, e1000Blob.bytes);

    const stackBlob = devicesRaw.find(
      (entry): entry is { kind: string; bytes: Uint8Array } =>
        !!entry &&
        typeof (entry as { kind?: unknown }).kind === "string" &&
        (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_NET_STACK_KIND &&
        (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
    );
    if (stackBlob) restoreNetStackDeviceState(opts.runtimes.netStack, stackBlob.bytes, { tcpRestorePolicy: "drop" });

    return {
      cpu: copyU8ToArrayBuffer(rec.cpu),
      mmu: copyU8ToArrayBuffer(rec.mmu),
      devices: devices.length ? devices : undefined,
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
    let usbBytes: Uint8Array | null = null;
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

      if (kind === VM_SNAPSHOT_DEVICE_USB_KIND) usbBytes = e.data;
      if (kind === VM_SNAPSHOT_DEVICE_E1000_KIND) e1000Bytes = e.data;
      if (kind === VM_SNAPSHOT_DEVICE_NET_STACK_KIND) stackBytes = e.data;

      devices.push({ kind, bytes: copyU8ToArrayBuffer(e.data) });
    }

    if (usbBytes) restoreUsbDeviceState(opts.runtimes, usbBytes);
    if (e1000Bytes) restoreNetE1000DeviceState(opts.runtimes.netE1000, e1000Bytes);
    if (stackBytes) restoreNetStackDeviceState(opts.runtimes.netStack, stackBytes, { tcpRestorePolicy: "drop" });

    return {
      cpu: copyU8ToArrayBuffer(rec.cpu),
      mmu: copyU8ToArrayBuffer(rec.mmu),
      devices: devices.length ? devices : undefined,
    };
  } finally {
    try {
      builder.free();
    } catch {
      // ignore
    }
  }
}
