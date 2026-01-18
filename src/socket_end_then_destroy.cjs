const stateBySocket = new WeakMap();

function endThenDestroyQuietly(socket, data, opts = {}) {
  if (!socket || typeof socket.end !== "function") return;
  if (stateBySocket.has(socket)) return;

  const timeoutMs = Number.isFinite(opts.timeoutMs) ? opts.timeoutMs : 2_000;

  let timer;
  let cleanedUp = false;
  const cleanup = () => {
    if (cleanedUp) return;
    cleanedUp = true;
    if (timer) clearTimeout(timer);
    try {
      socket.off?.("close", cleanup);
      socket.off?.("error", cleanup);
    } catch {
      // ignore
    }
    try {
      socket.removeListener?.("close", cleanup);
      socket.removeListener?.("error", cleanup);
    } catch {
      // ignore
    }
  };

  // Mark this socket as being torn down before calling `end()` to avoid reentrancy, and attach
  // listeners before `end()` to avoid races where `close` fires synchronously.
  stateBySocket.set(socket, true);
  try {
    socket.once?.("close", cleanup);
    socket.once?.("error", cleanup);
  } catch {
    // ignore
  }

  try {
    socket.end(data);
  } catch {
    cleanup();
    try {
      socket.destroy?.();
    } catch {
      // ignore
    }
    return;
  }

  if (cleanedUp) return;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) return;

  timer = setTimeout(() => {
    try {
      socket.destroy?.();
    } catch {
      // ignore
    }
    cleanup();
  }, timeoutMs);
  timer.unref?.();
}

module.exports = { endThenDestroyQuietly };
