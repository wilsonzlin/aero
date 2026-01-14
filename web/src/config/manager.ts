import {
  detectAeroBrowserCapabilities,
  parseAeroConfigOverrides,
  resolveAeroConfigFromSources,
  type AeroBrowserCapabilities,
  type AeroConfig,
  type AeroConfigKey,
  type ResolvedAeroConfig,
} from "./aero_config";
import { clearStoredAeroConfig, loadStoredAeroConfig, saveStoredAeroConfig } from "./storage";
import { readJsonResponseWithLimit } from "../storage/response_json";

// Deployment-provided config should be small; cap response size to avoid pathological allocations.
const MAX_STATIC_CONFIG_JSON_BYTES = 1024 * 1024; // 1 MiB

export interface AeroConfigManagerOptions {
  capabilities?: AeroBrowserCapabilities;
  storage?: Storage;
  queryString?: string;
  /**
   * Optional URL to a deployment-provided config file.
   *
   * If present and it loads successfully, it is treated as a low-precedence
   * override (above built-in defaults, below localStorage and URL params).
   */
  staticConfigUrl?: string;
}

export type AeroConfigListener = (state: ResolvedAeroConfig) => void;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

// Avoid prototype pollution when copying potentially-untrusted config objects.
//
// Setting `obj["__proto__"] = value` mutates the object's prototype. `defineProperty` always creates
// an own data property instead.
function safeRecordSet(record: Record<string, unknown>, key: string, value: unknown): void {
  Object.defineProperty(record, key, { value, enumerable: true, configurable: true, writable: true });
}

/**
 * Remove secrets that must never be persisted in localStorage.
 *
 * Note: `parseAeroConfigOverrides` intentionally ignores these keys, but if they
 * end up in storage (via a bug or manual insertion), we must not re-save them.
 */
function sanitizeStoredConfig(obj: Record<string, unknown>): boolean {
  let changed = false;
  if ("l2TunnelToken" in obj) {
    delete obj.l2TunnelToken;
    changed = true;
  }
  if ("l2TunnelTokenTransport" in obj) {
    delete obj.l2TunnelTokenTransport;
    changed = true;
  }
  return changed;
}

export class AeroConfigManager {
  private readonly capabilities: AeroBrowserCapabilities;
  private readonly storage: Storage | undefined;
  private readonly queryString: string;
  private readonly staticConfigUrl: string | undefined;

  private staticConfig: unknown = null;
  // Use a null prototype so inherited `Object.prototype.*` values cannot affect config storage
  // semantics (and so keys like "__proto__" are never treated as prototype setters).
  private storedConfig: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
  private state: ResolvedAeroConfig;
  private readonly listeners = new Set<AeroConfigListener>();

  constructor(options: AeroConfigManagerOptions = {}) {
    this.capabilities = options.capabilities ?? detectAeroBrowserCapabilities();
    this.storage = options.storage ?? safeGetLocalStorage();
    this.queryString = options.queryString ?? safeGetLocationSearch();
    this.staticConfigUrl = options.staticConfigUrl;

    const rawStored = loadStoredAeroConfig(this.storage);
    if (isRecord(rawStored)) {
      // Copy into a null-prototype object so stored config never observes inherited keys, and so
      // malicious `"__proto__"` entries in localStorage cannot mutate the prototype chain.
      this.storedConfig = Object.create(null) as Record<string, unknown>;
      for (const [k, v] of Object.entries(rawStored)) {
        safeRecordSet(this.storedConfig, k, v);
      }
      const scrubbed = sanitizeStoredConfig(this.storedConfig);
      // If secrets were present in persistent storage, re-save immediately so
      // they are removed from localStorage without waiting for a later update.
      if (scrubbed) {
        saveStoredAeroConfig(this.storedConfig, this.storage);
      }
    }

    this.state = resolveAeroConfigFromSources({
      capabilities: this.capabilities,
      staticConfig: this.staticConfig,
      storedConfig: this.storedConfig,
      queryString: this.queryString,
    });
  }

  /**
   * Loads the optional deployment config file (if configured), then notifies listeners.
   */
  async init(): Promise<void> {
    if (!this.staticConfigUrl) {
      this.emit();
      return;
    }

    try {
      const res = await fetch(this.staticConfigUrl, { cache: "no-store" });
      if (!res.ok) {
        this.emit();
        return;
      }
      this.staticConfig = await readJsonResponseWithLimit(res, {
        maxBytes: MAX_STATIC_CONFIG_JSON_BYTES,
        label: "static config",
      });
    } catch {
      // Ignore; optional.
    }

    this.recompute();
    this.emit();
  }

  getState(): ResolvedAeroConfig {
    return this.state;
  }

  subscribe(listener: AeroConfigListener): () => void {
    this.listeners.add(listener);
    listener(this.state);
    return () => {
      this.listeners.delete(listener);
    };
  }

  updateStoredConfig(patch: Partial<AeroConfig>): void {
    const blockedKeys = this.state.lockedKeys;
    // Use a null-prototype patch object so prototype pollution keys (e.g. "__proto__") never
    // mutate the patch object's prototype.
    const nextPatch: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
    for (const [k, v] of Object.entries(patch) as [AeroConfigKey, AeroConfig[AeroConfigKey]][]) {
      if (blockedKeys.has(k)) continue;
      safeRecordSet(nextPatch, k, v);
    }

    const parsed = parseAeroConfigOverrides(nextPatch);
    // Only apply keys that are valid after parsing; invalid inputs should be handled by the UI.
    for (const [k, v] of Object.entries(parsed.overrides as Record<string, unknown>)) {
      safeRecordSet(this.storedConfig, k, v);
    }
    sanitizeStoredConfig(this.storedConfig);
    saveStoredAeroConfig(this.storedConfig, this.storage);

    this.recompute();
    this.emit();
  }

  resetToDefaults(): void {
    this.storedConfig = Object.create(null) as Record<string, unknown>;
    clearStoredAeroConfig(this.storage);
    this.recompute();
    this.emit();
  }

  private recompute(): void {
    this.state = resolveAeroConfigFromSources({
      capabilities: this.capabilities,
      staticConfig: this.staticConfig,
      storedConfig: this.storedConfig,
      queryString: this.queryString,
    });
  }

  private emit(): void {
    for (const listener of this.listeners) {
      listener(this.state);
    }
  }
}

function safeGetLocalStorage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

function safeGetLocationSearch(): string {
  try {
    return globalThis.location?.search ?? "";
  } catch {
    return "";
  }
}
