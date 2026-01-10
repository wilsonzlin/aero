import type { AeroConfig } from "./aero_config";

export const AERO_CONFIG_STORAGE_KEY = "aero:config:v1";

function getDefaultStorage(): Storage | undefined {
  // `localStorage` might throw in some sandboxed contexts.
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

export function loadStoredAeroConfig(storage: Storage | undefined = getDefaultStorage()): unknown {
  if (!storage) return null;
  try {
    const raw = storage.getItem(AERO_CONFIG_STORAGE_KEY);
    if (raw === null) return null;
    return JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
}

export function saveStoredAeroConfig(
  overrides: Partial<AeroConfig>,
  storage: Storage | undefined = getDefaultStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(AERO_CONFIG_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // Ignore quota/security failures; settings remain in-memory for this session.
  }
}

export function clearStoredAeroConfig(storage: Storage | undefined = getDefaultStorage()): void {
  if (!storage) return;
  try {
    storage.removeItem(AERO_CONFIG_STORAGE_KEY);
  } catch {
    // Ignore.
  }
}
