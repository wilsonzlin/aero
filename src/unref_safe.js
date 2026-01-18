import { tryGetProp } from "./safe_props.js";

export function unrefBestEffort(handle) {
  if (handle == null || (typeof handle !== "object" && typeof handle !== "function")) return;
  const unref = tryGetProp(handle, "unref");
  if (typeof unref !== "function") return;
  try {
    unref.call(handle);
  } catch {
    // ignore
  }
}

