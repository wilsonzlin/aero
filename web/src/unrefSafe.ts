export function unrefBestEffort(handle: unknown): void {
  if (handle == null || (typeof handle !== "object" && typeof handle !== "function")) return;

  let unref: unknown;
  try {
    unref = (handle as { unref?: unknown }).unref;
  } catch {
    return;
  }

  if (typeof unref !== "function") return;
  try {
    (unref as (this: unknown) => void).call(handle);
  } catch {
    // ignore
  }
}

