import { callMethodBestEffort, destroyBestEffort, tryGetMethodBestEffort } from "./socket_safe.js";
import { unrefBestEffort } from "./unref_safe.js";

const socketsBeingDestroyed = new WeakSet();

export function endThenDestroyQuietly(socket, data, opts = {}) {
  const end = tryGetMethodBestEffort(socket, "end");
  if (!end) return;
  if (socketsBeingDestroyed.has(socket)) return;

  const timeoutMs = Number.isFinite(opts.timeoutMs) ? opts.timeoutMs : 2_000;

  let timer;
  let cleanedUp = false;
  const cleanup = () => {
    if (cleanedUp) return;
    cleanedUp = true;
    if (timer) clearTimeout(timer);
    callMethodBestEffort(socket, "off", "close", cleanup);
    callMethodBestEffort(socket, "off", "error", cleanup);
    callMethodBestEffort(socket, "removeListener", "close", cleanup);
    callMethodBestEffort(socket, "removeListener", "error", cleanup);
  };

  // Mark this socket as being torn down before calling `end()` to avoid reentrancy, and attach
  // listeners before `end()` to avoid races where `close` fires synchronously.
  socketsBeingDestroyed.add(socket);
  callMethodBestEffort(socket, "once", "close", cleanup);
  callMethodBestEffort(socket, "once", "error", cleanup);

  try {
    end.call(socket, data);
  } catch {
    cleanup();
    destroyBestEffort(socket);
    return;
  }

  if (cleanedUp) return;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) return;

  timer = setTimeout(() => {
    destroyBestEffort(socket);
    cleanup();
  }, timeoutMs);
  unrefBestEffort(timer);
}
