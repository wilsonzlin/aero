import type { Duplex } from "node:stream";

import { endThenDestroyQuietly } from "../../../../src/socket_end_then_destroy.js";

type GracefulDuplexCloser = Readonly<{
  endThenDestroy: () => void;
  destroyNow: () => void;
}>;

export function createGracefulDuplexCloser(
  socket: Duplex,
  opts: Readonly<{ timeoutMs?: number }> = {},
): GracefulDuplexCloser {
  const destroyNow = () => {
    try {
      socket.destroy();
    } catch {
      // ignore
    }
  };

  const endThenDestroy = () => {
    endThenDestroyQuietly(socket, undefined, { timeoutMs: opts.timeoutMs });
  };

  return Object.freeze({ endThenDestroy, destroyNow });
}
