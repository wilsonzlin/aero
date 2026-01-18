import type { Duplex } from "node:stream";

import { endThenDestroyQuietly } from "./socketEndThenDestroy.js";
import { destroyBestEffort } from "./socketSafe.js";

type GracefulDuplexCloser = Readonly<{
  endThenDestroy: () => void;
  destroyNow: () => void;
}>;

export function createGracefulDuplexCloser(
  socket: Duplex,
  opts: Readonly<{ timeoutMs?: number }> = {},
): GracefulDuplexCloser {
  const destroyNow = () => {
    destroyBestEffort(socket);
  };

  const endThenDestroy = () => {
    endThenDestroyQuietly(socket, undefined, { timeoutMs: opts.timeoutMs });
  };

  return Object.freeze({ endThenDestroy, destroyNow });
}
