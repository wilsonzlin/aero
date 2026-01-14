import type { DiskKind } from "../storage/metadata";
import { deserializeRuntimeDiskSnapshot } from "../storage/runtime_disk_snapshot";
import { VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID } from "./vm_snapshot_wasm";

/**
 * IO worker integration: persist RuntimeDiskWorker state (overlay refs + remote cache bindings)
 * inside the VM snapshot file.
 *
 * This is *host-side* state and is not part of the guest-visible disk controller snapshot. We
 * encode it as an opaque `device.<id>` blob so older WASM snapshot builds can roundtrip it.
 */
export const IO_WORKER_RUNTIME_DISK_SNAPSHOT_DEVICE_ID = 1_000_000_000;
export const IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND = `${VM_SNAPSHOT_DEVICE_KIND_PREFIX_ID}${IO_WORKER_RUNTIME_DISK_SNAPSHOT_DEVICE_ID}`;

export type RuntimeDiskClientSnapshotLike = {
  prepareSnapshot(): Promise<Uint8Array>;
  restoreFromSnapshot(state: Uint8Array): Promise<void>;
};

export type RuntimeDiskSnapshotDiskInfo = {
  handle: number;
  sectorSize: number;
  capacityBytes: number;
  readOnly: boolean;
};

export async function appendRuntimeDiskWorkerSnapshotDeviceBlob(
  devices: Array<{ kind: string; bytes: Uint8Array }>,
  diskClient: RuntimeDiskClientSnapshotLike | null,
): Promise<void> {
  if (!diskClient) return;
  const state = await diskClient.prepareSnapshot();
  devices.push({ kind: IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND, bytes: state });
}

export function findRuntimeDiskWorkerSnapshotDeviceBlob(devices: Iterable<{ kind: string; bytes: Uint8Array }>): Uint8Array | null {
  for (const dev of devices) {
    if (dev.kind === IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND) return dev.bytes;
  }
  return null;
}

export function decodeRuntimeDiskWorkerSnapshotActiveDisks(state: Uint8Array): {
  activeDisk: RuntimeDiskSnapshotDiskInfo | null;
  cdDisk: RuntimeDiskSnapshotDiskInfo | null;
} {
  const snapshot = deserializeRuntimeDiskSnapshot(state);
  let hdd: RuntimeDiskSnapshotDiskInfo | null = null;
  let cd: RuntimeDiskSnapshotDiskInfo | null = null;

  for (const disk of snapshot.disks) {
    const kind = (disk.backend as { diskKind?: unknown }).diskKind as DiskKind | undefined;
    const info: RuntimeDiskSnapshotDiskInfo = {
      handle: disk.handle,
      sectorSize: disk.sectorSize,
      capacityBytes: disk.capacityBytes,
      readOnly: disk.readOnly,
    };
    if (kind === "hdd" && !hdd) hdd = info;
    if (kind === "cd" && !cd) cd = info;
  }

  return { activeDisk: hdd ?? cd, cdDisk: cd };
}

export async function restoreRuntimeDiskWorkerSnapshotFromDeviceBlobs(opts: {
  devices: Iterable<{ kind: string; bytes: Uint8Array }>;
  diskClient: RuntimeDiskClientSnapshotLike;
}): Promise<{
  state: Uint8Array;
  activeDisk: RuntimeDiskSnapshotDiskInfo | null;
  cdDisk: RuntimeDiskSnapshotDiskInfo | null;
} | null> {
  const state = findRuntimeDiskWorkerSnapshotDeviceBlob(opts.devices);
  if (!state) return null;
  // Decode before handing off to `RuntimeDiskClient.restoreFromSnapshot`, which may transfer
  // (detach) the underlying ArrayBuffer for zero-copy IPC.
  const { activeDisk, cdDisk } = decodeRuntimeDiskWorkerSnapshotActiveDisks(state);
  // Use a copy so that callers (and cached `restoredDevices`) retain the original bytes.
  await opts.diskClient.restoreFromSnapshot(state.slice());
  return { state, activeDisk, cdDisk };
}
