import type { WasmApi } from "../runtime/wasm_loader";

export const VM_SNAPSHOT_DEVICE_USB_KIND = "usb.uhci";
export const VM_SNAPSHOT_DEVICE_I8042_KIND = "input.i8042";
export const VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND = "audio.hda";

// `aero_snapshot::DeviceId::USB` (see `docs/16-snapshots.md`).
export const VM_SNAPSHOT_DEVICE_ID_USB = 12;
// `aero_snapshot::DeviceId::I8042` (see `docs/16-snapshots.md`).
// NOTE: This must match the Rust `DeviceId` assignment.
export const VM_SNAPSHOT_DEVICE_ID_I8042 = 13;
// `aero_snapshot::DeviceId::HDA` (see `docs/16-snapshots.md`).
export const VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA = 18;

export const VM_SNAPSHOT_SAVE_TO_OPFS_EXPORT_NAMES = [
  // Preferred names (Task 1078).
  "vm_snapshot_save_to_opfs",
  "save_vm_snapshot_to_opfs",
  // Legacy/alternate spellings.
  "snapshot_vm_to_opfs",
  "snapshot_worker_vm_to_opfs",
  "worker_vm_snapshot_to_opfs",
] as const;

export const VM_SNAPSHOT_RESTORE_FROM_OPFS_EXPORT_NAMES = [
  // Preferred names (Task 1078).
  "vm_snapshot_restore_from_opfs",
  "restore_vm_snapshot_from_opfs",
  // Legacy/alternate spellings.
  "restore_snapshot_vm_from_opfs",
  "restore_worker_vm_snapshot_from_opfs",
  "snapshot_restore_vm_from_opfs",
] as const;

export type VmSnapshotSaveToOpfsExport =
  | { kind: "free-function"; fn: (...args: unknown[]) => unknown }
  | { kind: "builder"; Ctor: NonNullable<WasmApi["WorkerVmSnapshot"]> };

export type VmSnapshotRestoreFromOpfsExport =
  | { kind: "free-function"; fn: (...args: unknown[]) => unknown }
  | { kind: "builder"; Ctor: NonNullable<WasmApi["WorkerVmSnapshot"]> };

function resolveWasmVmSnapshotFn(api: WasmApi, names: readonly string[]): ((...args: unknown[]) => unknown) | null {
  const anyApi = api as unknown as Record<string, unknown>;
  for (const name of names) {
    const fn = anyApi[name];
    if (typeof fn === "function") return fn as (...args: unknown[]) => unknown;
  }
  return null;
}

function resolveWorkerVmSnapshotCtor(api: WasmApi): NonNullable<WasmApi["WorkerVmSnapshot"]> | null {
  const ctor = (api as unknown as { WorkerVmSnapshot?: unknown }).WorkerVmSnapshot;
  if (typeof ctor !== "function") return null;
  return ctor as NonNullable<WasmApi["WorkerVmSnapshot"]>;
}

export function resolveVmSnapshotSaveToOpfsExport(api: WasmApi): VmSnapshotSaveToOpfsExport | null {
  const fn = resolveWasmVmSnapshotFn(api, VM_SNAPSHOT_SAVE_TO_OPFS_EXPORT_NAMES);
  if (fn) return { kind: "free-function", fn };
  const Ctor = resolveWorkerVmSnapshotCtor(api);
  if (Ctor) return { kind: "builder", Ctor };
  return null;
}

export function resolveVmSnapshotRestoreFromOpfsExport(api: WasmApi): VmSnapshotRestoreFromOpfsExport | null {
  const fn = resolveWasmVmSnapshotFn(api, VM_SNAPSHOT_RESTORE_FROM_OPFS_EXPORT_NAMES);
  if (fn) return { kind: "free-function", fn };
  const Ctor = resolveWorkerVmSnapshotCtor(api);
  if (Ctor) return { kind: "builder", Ctor };
  return null;
}

export function parseAeroIoSnapshotVersion(bytes: Uint8Array): { version: number; flags: number } {
  // `aero-io-snapshot` TLV blobs begin with a 16-byte header:
  //   magic[4] = "AERO"
  //   format_version: u16 major, u16 minor
  //   device_id: [u8; 4]
  //   device_version: u16 major, u16 minor
  //
  // However, some legacy JS device snapshots (e.g. the JS i8042 controller model) also start with
  // "AERO" but use a different, shorter header:
  //   magic[4] = "AERO"
  //   version: u16
  //   flags: u16
  //
  // This helper tries to detect the `aero-io-snapshot` header by checking that the 4-byte device id
  // region looks like an ASCII tag, and falls back to the legacy header otherwise.
  if (bytes.byteLength >= 4 && bytes[0] === 0x41 && bytes[1] === 0x45 && bytes[2] === 0x52 && bytes[3] === 0x4f) {
    if (bytes.byteLength >= 16) {
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
        const major = (bytes[12]! | (bytes[13]! << 8)) >>> 0;
        const minor = (bytes[14]! | (bytes[15]! << 8)) >>> 0;
        return { version: major, flags: minor };
      }
    }

    if (bytes.byteLength >= 8) {
      const version = (bytes[4]! | (bytes[5]! << 8)) >>> 0;
      const flags = (bytes[6]! | (bytes[7]! << 8)) >>> 0;
      return { version, flags };
    }
  }

  // Fallback for unknown payloads: treat as v1 with no flags.
  return { version: 1, flags: 0 };
}

export function vmSnapshotDeviceIdToKind(id: number): string | null {
  if ((id >>> 0) === VM_SNAPSHOT_DEVICE_ID_USB) return VM_SNAPSHOT_DEVICE_USB_KIND;
  if ((id >>> 0) === VM_SNAPSHOT_DEVICE_ID_I8042) return VM_SNAPSHOT_DEVICE_I8042_KIND;
  if ((id >>> 0) === VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA) return VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND;
  return null;
}

export function vmSnapshotDeviceKindToId(kind: string): number | null {
  if (kind === VM_SNAPSHOT_DEVICE_USB_KIND) return VM_SNAPSHOT_DEVICE_ID_USB;
  if (kind === VM_SNAPSHOT_DEVICE_I8042_KIND) return VM_SNAPSHOT_DEVICE_ID_I8042;
  if (kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND) return VM_SNAPSHOT_DEVICE_ID_AUDIO_HDA;
  return null;
}
