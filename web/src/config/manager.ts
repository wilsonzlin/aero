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

export class AeroConfigManager {
  private readonly capabilities: AeroBrowserCapabilities;
  private readonly storage: Storage | undefined;
  private readonly queryString: string;
  private readonly staticConfigUrl: string | undefined;

  private staticConfig: unknown = null;
  private storedConfig: Record<string, unknown> = {};
  private state: ResolvedAeroConfig;
  private readonly listeners = new Set<AeroConfigListener>();

  constructor(options: AeroConfigManagerOptions = {}) {
    this.capabilities = options.capabilities ?? detectAeroBrowserCapabilities();
    this.storage = options.storage ?? safeGetLocalStorage();
    this.queryString = options.queryString ?? safeGetLocationSearch();
    this.staticConfigUrl = options.staticConfigUrl;

    const rawStored = loadStoredAeroConfig(this.storage);
    if (isRecord(rawStored)) {
      this.storedConfig = { ...rawStored };
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
      this.staticConfig = (await res.json()) as unknown;
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
    const nextPatch: Partial<AeroConfig> = {};
    for (const [k, v] of Object.entries(patch) as [AeroConfigKey, AeroConfig[AeroConfigKey]][]) {
      if (blockedKeys.has(k)) continue;
      (nextPatch as Record<string, unknown>)[k] = v;
    }

    const parsed = parseAeroConfigOverrides(nextPatch);
    // Only apply keys that are valid after parsing; invalid inputs should be handled by the UI.
    Object.assign(this.storedConfig, parsed.overrides as Record<string, unknown>);
    saveStoredAeroConfig(this.storedConfig, this.storage);

    this.recompute();
    this.emit();
  }

  resetToDefaults(): void {
    this.storedConfig = {};
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
