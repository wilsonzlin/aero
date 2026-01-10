import type { DiskImageStore } from "./disk_image_store";
import { MemoryDiskImageStore } from "./memory_disk_image_store";
import { getOpfsSupportStatus, OpfsDiskImageStore } from "./opfs_disk_image_store";

export function createDefaultDiskImageStore(): {
  store: DiskImageStore;
  persistent: boolean;
  warning?: string;
} {
  const status = getOpfsSupportStatus();
  if (status.supported) {
    return { store: new OpfsDiskImageStore(), persistent: true };
  }
  return { store: new MemoryDiskImageStore(), persistent: false, warning: status.reason };
}

