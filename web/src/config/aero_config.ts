import { PCI_MMIO_BASE_MIB } from "../arch/guest_phys.ts";

export const AERO_LOG_LEVELS = ["trace", "debug", "info", "warn", "error"] as const;
export type AeroLogLevel = (typeof AERO_LOG_LEVELS)[number];

export interface AeroConfig {
  guestMemoryMiB: number;
  enableWorkers: boolean;
  enableWebGPU: boolean;
  proxyUrl: string | null;
  activeDiskImage: string | null;
  logLevel: AeroLogLevel;
  uiScale?: number;
}

export type AeroConfigKey = keyof AeroConfig;

export const AERO_GUEST_MEMORY_MIN_MIB = 256;
export const AERO_GUEST_MEMORY_MAX_MIB = PCI_MMIO_BASE_MIB;
export const AERO_GUEST_MEMORY_PRESETS_MIB = [256, 512, 1024, 2048, 3072, AERO_GUEST_MEMORY_MAX_MIB] as const;

export const AERO_UI_SCALE_MIN = 0.5;
export const AERO_UI_SCALE_MAX = 3;

export interface AeroBrowserCapabilities {
  supportsThreadedWorkers: boolean;
  threadedWorkersUnsupportedReason: string | null;
  supportsWebGPU: boolean;
  webgpuUnsupportedReason: string | null;
}

export interface AeroConfigValidationIssue {
  key: AeroConfigKey;
  message: string;
}

export interface ParsedAeroConfigOverrides {
  overrides: Partial<AeroConfig>;
  issues: AeroConfigValidationIssue[];
}

export interface ParsedAeroQueryOverrides extends ParsedAeroConfigOverrides {
  lockedKeys: Set<AeroConfigKey>;
}

export interface ResolvedAeroConfig {
  capabilities: AeroBrowserCapabilities;
  defaults: AeroConfig;
  requested: AeroConfig;
  effective: AeroConfig;
  lockedKeys: Set<AeroConfigKey>;
  forced: Partial<Record<AeroConfigKey, string>>;
  issues: AeroConfigValidationIssue[];
  layers: {
    static: ParsedAeroConfigOverrides;
    stored: ParsedAeroConfigOverrides;
    query: ParsedAeroQueryOverrides;
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function hasOwn(obj: Record<string, unknown>, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function clampInt(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.trunc(value)));
}

function parseBoolean(value: unknown): boolean | undefined {
  if (typeof value === "boolean") {
    return value;
  }
  if (typeof value === "number") {
    if (value === 1) return true;
    if (value === 0) return false;
    return undefined;
  }
  if (typeof value === "string") {
    const v = value.trim().toLowerCase();
    if (["1", "true", "yes", "y", "on"].includes(v)) return true;
    if (["0", "false", "no", "n", "off"].includes(v)) return false;
  }
  return undefined;
}

function parseNullableString(value: unknown): string | null | undefined {
  if (value === null) {
    return null;
  }
  if (typeof value === "string") {
    const v = value.trim();
    if (v === "" || v.toLowerCase() === "null") return null;
    return v;
  }
  return undefined;
}

function parseLogLevel(value: unknown): AeroLogLevel | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if ((AERO_LOG_LEVELS as readonly string[]).includes(v)) {
    return v as AeroLogLevel;
  }
  return undefined;
}

export function detectAeroBrowserCapabilities(): AeroBrowserCapabilities {
  const hasWorker = typeof Worker !== "undefined";
  const hasSAB = typeof SharedArrayBuffer !== "undefined";
  const hasAtomics = typeof Atomics !== "undefined";
  const crossOriginIsolated = globalThis.crossOriginIsolated === true;

  let threadedWorkersUnsupportedReason: string | null = null;
  if (!hasWorker) {
    threadedWorkersUnsupportedReason = "Web Workers are not available in this environment.";
  } else if (!crossOriginIsolated) {
    threadedWorkersUnsupportedReason =
      "SharedArrayBuffer requires cross-origin isolation (COOP+COEP headers).";
  } else if (!hasSAB) {
    threadedWorkersUnsupportedReason = "SharedArrayBuffer is not available.";
  } else if (!hasAtomics) {
    threadedWorkersUnsupportedReason = "Atomics are not available.";
  }

  const supportsThreadedWorkers =
    hasWorker && hasSAB && hasAtomics && crossOriginIsolated && threadedWorkersUnsupportedReason === null;

  const webgpu = typeof navigator !== "undefined" && !!(navigator as Navigator & { gpu?: unknown }).gpu;
  const webgpuUnsupportedReason = webgpu ? null : "WebGPU is not available in this browser.";

  return {
    supportsThreadedWorkers,
    threadedWorkersUnsupportedReason,
    supportsWebGPU: webgpu,
    webgpuUnsupportedReason,
  };
}

export function getDefaultAeroConfig(
  capabilities: Partial<Pick<AeroBrowserCapabilities, "supportsThreadedWorkers" | "supportsWebGPU">> = {},
): AeroConfig {
  const enableWorkers = capabilities.supportsThreadedWorkers ?? true;
  const enableWebGPU = capabilities.supportsWebGPU ?? false;

  return {
    guestMemoryMiB: 512,
    enableWorkers,
    enableWebGPU,
    proxyUrl: null,
    activeDiskImage: null,
    logLevel: "info",
  };
}

function parseGuestMemoryMiB(value: unknown): { value: number } | { issue: string; value: number } | null {
  const num = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
  if (!Number.isFinite(num)) return null;

  const clamped = clampInt(num, AERO_GUEST_MEMORY_MIN_MIB, AERO_GUEST_MEMORY_MAX_MIB);
  if (clamped !== Math.trunc(num)) {
    return {
      issue: `guestMemoryMiB must be an integer between ${AERO_GUEST_MEMORY_MIN_MIB} and ${AERO_GUEST_MEMORY_MAX_MIB} MiB (clamped to ${clamped}).`,
      value: clamped,
    };
  }
  return { value: clamped };
}

function parseUiScale(value: unknown): { value: number } | { issue: string; value: number } | null {
  if (value === undefined || value === null) return null;
  const num = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
  if (!Number.isFinite(num)) return null;

  const clamped = Math.min(AERO_UI_SCALE_MAX, Math.max(AERO_UI_SCALE_MIN, num));
  if (clamped !== num) {
    return {
      issue: `uiScale must be between ${AERO_UI_SCALE_MIN} and ${AERO_UI_SCALE_MAX} (clamped to ${clamped}).`,
      value: clamped,
    };
  }

  return { value: clamped };
}

export function parseAndValidateProxyUrl(
  value: unknown,
): { proxyUrl: string | null } | { proxyUrl: string | null; issue: string } | null {
  const v = parseNullableString(value);
  if (v === undefined) return null;
  if (v === null) return { proxyUrl: null };

  // Support same-origin relative paths (common for deployments where the gateway
  // is hosted alongside the web app).
  if (v.startsWith("/")) {
    return { proxyUrl: v };
  }

  try {
    const parsed = new URL(v);
    if (parsed.protocol !== "ws:" && parsed.protocol !== "wss:" && parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return {
        proxyUrl: null,
        issue: "proxyUrl must be a ws://, wss://, http://, https://, or /path URL.",
      };
    }
    return { proxyUrl: v };
  } catch {
    return { proxyUrl: null, issue: "proxyUrl is not a valid URL." };
  }
}

export function parseAeroConfigOverrides(input: unknown): ParsedAeroConfigOverrides {
  const overrides: Partial<AeroConfig> = {};
  const issues: AeroConfigValidationIssue[] = [];

  if (!isRecord(input)) {
    return { overrides, issues };
  }

  if (hasOwn(input, "guestMemoryMiB")) {
    const parsed = parseGuestMemoryMiB(input.guestMemoryMiB);
    if (parsed) {
      overrides.guestMemoryMiB = parsed.value;
      if ("issue" in parsed) issues.push({ key: "guestMemoryMiB", message: parsed.issue });
    }
  }

  if (hasOwn(input, "enableWorkers")) {
    const parsed = parseBoolean(input.enableWorkers);
    if (parsed !== undefined) overrides.enableWorkers = parsed;
  }

  if (hasOwn(input, "enableWebGPU")) {
    const parsed = parseBoolean(input.enableWebGPU);
    if (parsed !== undefined) overrides.enableWebGPU = parsed;
  }

  if (hasOwn(input, "proxyUrl")) {
    const parsed = parseAndValidateProxyUrl(input.proxyUrl);
    if (parsed) {
      overrides.proxyUrl = parsed.proxyUrl;
      if ("issue" in parsed) issues.push({ key: "proxyUrl", message: parsed.issue });
    }
  }

  if (hasOwn(input, "activeDiskImage")) {
    const parsed = parseNullableString(input.activeDiskImage);
    if (parsed !== undefined) overrides.activeDiskImage = parsed;
  }

  if (hasOwn(input, "logLevel")) {
    const parsed = parseLogLevel(input.logLevel);
    if (parsed !== undefined) overrides.logLevel = parsed;
  }

  if (hasOwn(input, "uiScale")) {
    const parsed = parseUiScale(input.uiScale);
    if (parsed) {
      overrides.uiScale = parsed.value;
      if ("issue" in parsed) issues.push({ key: "uiScale", message: parsed.issue });
    }
  }

  return { overrides, issues };
}

export function parseAeroConfigQueryOverrides(search: string): ParsedAeroQueryOverrides {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  const overrides: Partial<AeroConfig> = {};
  const issues: AeroConfigValidationIssue[] = [];
  const lockedKeys = new Set<AeroConfigKey>();

  const mem = params.get("mem");
  if (mem !== null) {
    const parsed = parseGuestMemoryMiB(mem);
    if (parsed) {
      overrides.guestMemoryMiB = parsed.value;
      lockedKeys.add("guestMemoryMiB");
      if ("issue" in parsed) issues.push({ key: "guestMemoryMiB", message: parsed.issue });
    }
  }

  const workers = params.get("workers");
  if (workers !== null) {
    const parsed = parseBoolean(workers);
    if (parsed !== undefined) {
      overrides.enableWorkers = parsed;
      lockedKeys.add("enableWorkers");
    }
  }

  const webgpu = params.get("webgpu");
  if (webgpu !== null) {
    const parsed = parseBoolean(webgpu);
    if (parsed !== undefined) {
      overrides.enableWebGPU = parsed;
      lockedKeys.add("enableWebGPU");
    }
  }

  const proxy = params.get("proxy");
  if (proxy !== null) {
    const parsed = parseAndValidateProxyUrl(proxy);
    if (parsed) {
      if ("issue" in parsed) {
        issues.push({ key: "proxyUrl", message: parsed.issue });
      } else {
        overrides.proxyUrl = parsed.proxyUrl;
        lockedKeys.add("proxyUrl");
      }
    }
  }

  const disk = params.get("disk");
  if (disk !== null) {
    const parsed = parseNullableString(disk);
    if (parsed !== undefined) {
      overrides.activeDiskImage = parsed;
      lockedKeys.add("activeDiskImage");
    }
  }

  const log = params.get("log");
  if (log !== null) {
    const parsed = parseLogLevel(log);
    if (parsed !== undefined) {
      overrides.logLevel = parsed;
      lockedKeys.add("logLevel");
    }
  }

  const scale = params.get("scale");
  if (scale !== null) {
    const parsed = parseUiScale(scale);
    if (parsed) {
      overrides.uiScale = parsed.value;
      lockedKeys.add("uiScale");
      if ("issue" in parsed) issues.push({ key: "uiScale", message: parsed.issue });
    }
  }

  return { overrides, issues, lockedKeys };
}

export function applyAeroBrowserCapabilities(
  config: AeroConfig,
  capabilities: AeroBrowserCapabilities,
): { config: AeroConfig; forced: Partial<Record<AeroConfigKey, string>> } {
  const forced: Partial<Record<AeroConfigKey, string>> = {};
  const next: AeroConfig = { ...config };

  if (next.enableWorkers && !capabilities.supportsThreadedWorkers) {
    next.enableWorkers = false;
    forced.enableWorkers =
      capabilities.threadedWorkersUnsupportedReason ?? "Threaded workers are not supported in this browser.";
  }

  if (next.enableWebGPU && !capabilities.supportsWebGPU) {
    next.enableWebGPU = false;
    forced.enableWebGPU = capabilities.webgpuUnsupportedReason ?? "WebGPU is not supported in this browser.";
  }

  return { config: next, forced };
}

export function resolveAeroConfigFromSources(args: {
  capabilities: AeroBrowserCapabilities;
  staticConfig?: unknown;
  storedConfig?: unknown;
  queryString?: string;
}): ResolvedAeroConfig {
  const defaults = getDefaultAeroConfig(args.capabilities);

  const staticLayer = parseAeroConfigOverrides(args.staticConfig);
  const storedLayer = parseAeroConfigOverrides(args.storedConfig);
  const queryLayer = parseAeroConfigQueryOverrides(args.queryString ?? "");

  const requested: AeroConfig = {
    ...defaults,
    ...staticLayer.overrides,
    ...storedLayer.overrides,
  };

  const { config: effectivePreCaps, forced: forcedPreCaps } = applyAeroBrowserCapabilities(
    {
      ...requested,
      ...queryLayer.overrides,
    },
    args.capabilities,
  );

  const issues = [...staticLayer.issues, ...storedLayer.issues, ...queryLayer.issues];

  return {
    capabilities: args.capabilities,
    defaults,
    requested,
    effective: effectivePreCaps,
    forced: forcedPreCaps,
    lockedKeys: queryLayer.lockedKeys,
    issues,
    layers: {
      static: staticLayer,
      stored: storedLayer,
      query: queryLayer,
    },
  };
}
