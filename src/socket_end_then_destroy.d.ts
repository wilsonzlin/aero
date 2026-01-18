import type { Duplex } from "node:stream";

export function endThenDestroyQuietly(
  socket: Duplex | null | undefined,
  data?: string | Uint8Array,
  opts?: Readonly<{ timeoutMs?: number }>,
): void;
