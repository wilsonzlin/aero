import { PCI_MMIO_BASE_MIB } from "../arch/guest_phys.ts";
import type { InputBackendOverride } from "../input/input_backend_selection.ts";
import type { VirtioInputPciMode } from "../io/devices/virtio_input.ts";
import type { VirtioNetPciMode } from "../io/devices/virtio_net.ts";
import type { VirtioSndPciMode } from "../io/devices/virtio_snd.ts";
import type { L2TunnelTokenTransport } from "../net/l2Tunnel";
import type { L2TunnelTransportMode } from "../net/connectL2Tunnel";
import type { RelaySignalingMode } from "../net/webrtcRelaySignalingClient";

export const AERO_LOG_LEVELS = ["trace", "debug", "info", "warn", "error"] as const;
export type AeroLogLevel = (typeof AERO_LOG_LEVELS)[number];

export const AERO_VM_RUNTIMES = ["legacy", "machine"] as const;
export type AeroVmRuntime = (typeof AERO_VM_RUNTIMES)[number];

export interface AeroConfig {
  guestMemoryMiB: number;
  enableWorkers: boolean;
  enableWebGPU: boolean;
  proxyUrl: string | null;
  /**
   * Underlying transport for the L2 tunnel (Option C networking).
   *
   * - `"ws"` (default): direct WebSocket tunnel to the gateway `/l2` endpoint.
   * - `"webrtc"`: WebRTC `RTCDataChannel` tunnel via the UDP relay returned by `POST /session`.
   */
  l2TunnelTransport?: L2TunnelTransportMode;
  /**
   * Advanced: signaling mode for the WebRTC relay (only relevant when
   * `l2TunnelTransport="webrtc"`).
   *
   * Default: `"ws-trickle"`.
   */
  l2RelaySignalingMode?: RelaySignalingMode;
  /**
   * Optional token for deployments that require auth on the `/l2` WebSocket endpoint.
   *
   * This is forwarded to the net worker and passed to `WebSocketL2TunnelClient`.
   *
   * Note: This config surface is intentionally optional; absent values preserve the
   * current unauthenticated behavior.
   */
  l2TunnelToken?: string;
  /**
   * How to transport `l2TunnelToken` to the `/l2` endpoint (WebSocket only).
   *
   * See `L2TunnelTokenTransport` / `WebSocketL2TunnelClient` for details.
   *
   * Default: `"query"`.
  */
  l2TunnelTokenTransport?: L2TunnelTokenTransport;
  activeDiskImage: string | null;
  logLevel: AeroLogLevel;
  /**
   * Selects which VM runtime implementation to use:
   *
   * - "legacy": CPU-only `WasmVm` + JS I/O shims (status quo)
   * - "machine": canonical full-system `api.Machine` with AHCI/IDE
   *
   * Defaults to "legacy".
   */
  vmRuntime?: AeroVmRuntime;
  uiScale?: number;
  /**
   * Overrides the keyboard input backend selection in the IO worker.
   *
   * - "auto": use the default selection logic (virtio → usb → ps2)
   * - "ps2": always inject via i8042 PS/2
   * - "usb": always inject via synthetic USB HID (when available)
   * - "virtio": always inject via virtio-input (when available)
   *
   * When the forced backend is not currently available (e.g. virtio before DRIVER_OK),
   * the IO worker will fall back to "auto" and emit a one-time warning.
   */
  forceKeyboardBackend?: InputBackendOverride;
  /**
   * Overrides the mouse input backend selection in the IO worker.
   *
   * See `forceKeyboardBackend` for semantics.
   */
  forceMouseBackend?: InputBackendOverride;
  /**
   * Selects which virtio-pci transport to expose for virtio-net:
   * - "modern": modern-only (Aero contract v1, status quo)
   * - "transitional": modern virtio-pci + legacy I/O port BAR (virtio-win compatibility)
   * - "legacy": legacy-only virtio-pci (forces legacy driver path)
   *
   * Defaults to "modern".
   */
  virtioNetMode?: VirtioNetPciMode;
  /**
   * Selects which virtio-pci transport to expose for virtio-input:
   * - "modern": modern-only (Aero contract v1, status quo)
   * - "transitional": modern virtio-pci + legacy I/O port BAR (virtio-win compatibility)
   * - "legacy": legacy-only virtio-pci (forces legacy driver path)
   *
   * Defaults to "modern".
   */
  virtioInputMode?: VirtioInputPciMode;
  /**
   * Selects which virtio-pci transport to expose for virtio-snd:
   * - "modern": modern-only (Aero contract v1, status quo)
   * - "transitional": modern virtio-pci + legacy I/O port BAR (virtio-win compatibility)
   * - "legacy": legacy-only virtio-pci (forces legacy driver path)
   *
   * Defaults to "modern".
   */
  virtioSndMode?: VirtioSndPciMode;
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

function parseNonEmptyString(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim();
  if (v === "") return undefined;
  return v;
}

function parseLogLevel(value: unknown): AeroLogLevel | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if ((AERO_LOG_LEVELS as readonly string[]).includes(v)) {
    return v as AeroLogLevel;
  }
  return undefined;
}

function parseVmRuntime(value: unknown): AeroVmRuntime | undefined {
  if (typeof value === "string") {
    const v = value.trim().toLowerCase();
    if (v === "legacy") return "legacy";
    if (v === "machine") return "machine";
    return undefined;
  }
  if (typeof value === "number") {
    if (value === 0) return "legacy";
    if (value === 1) return "machine";
    return undefined;
  }
  if (typeof value === "boolean") {
    return value ? "machine" : "legacy";
  }
  return undefined;
}

function parseVirtioPciMode(value: unknown): "modern" | "transitional" | "legacy" | undefined {
  if (typeof value === "string") {
    const v = value.trim().toLowerCase();
    if (v === "modern") return "modern";
    if (v === "transitional" || v === "transition") return "transitional";
    if (v === "legacy" || v === "legacy-only") return "legacy";
    return undefined;
  }
  if (typeof value === "number") {
    if (value === 0) return "modern";
    if (value === 1) return "transitional";
    if (value === 2) return "legacy";
    return undefined;
  }
  if (typeof value === "boolean") {
    return value ? "transitional" : "modern";
  }
  return undefined;
}

function parseL2TunnelTransport(value: unknown): L2TunnelTransportMode | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if (v === "ws" || v === "websocket" || v === "web-socket") return "ws";
  if (v === "webrtc" || v === "rtc" || v === "web-rtc") return "webrtc";
  return undefined;
}

function parseRelaySignalingMode(value: unknown): RelaySignalingMode | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if (v === "ws-trickle" || v === "trickle" || v === "ws") return "ws-trickle";
  if (v === "http-offer" || v === "http") return "http-offer";
  if (v === "legacy-offer" || v === "legacy") return "legacy-offer";
  return undefined;
}

function parseL2TunnelTokenTransport(value: unknown): L2TunnelTokenTransport | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if (v === "query" || v === "url") return "query";
  if (v === "subprotocol" || v === "subproto" || v === "protocol") return "subprotocol";
  if (v === "both") return "both";
  return undefined;
}

function parseInputBackendOverride(value: unknown): InputBackendOverride | undefined {
  if (typeof value !== "string") return undefined;
  const v = value.trim().toLowerCase();
  if (v === "" || v === "default") return "auto";
  if (v === "auto") return "auto";
  if (v === "ps2" || v === "i8042") return "ps2";
  if (v === "usb" || v === "hid" || v === "usbhid") return "usb";
  if (v === "virtio" || v === "virtio-input" || v === "virtio_input") return "virtio";
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
    l2TunnelTransport: "ws",
    l2RelaySignalingMode: "ws-trickle",
    l2TunnelTokenTransport: "query",
    vmRuntime: "legacy",
    virtioNetMode: "modern",
    virtioInputMode: "modern",
    virtioSndMode: "modern",
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

  if (hasOwn(input, "l2TunnelTransport")) {
    const parsed = parseL2TunnelTransport(input.l2TunnelTransport);
    if (parsed !== undefined) {
      overrides.l2TunnelTransport = parsed;
    } else {
      issues.push({ key: "l2TunnelTransport", message: 'l2TunnelTransport must be "ws" or "webrtc".' });
    }
  }

  if (hasOwn(input, "l2RelaySignalingMode")) {
    const parsed = parseRelaySignalingMode(input.l2RelaySignalingMode);
    if (parsed !== undefined) {
      overrides.l2RelaySignalingMode = parsed;
    } else {
      issues.push({
        key: "l2RelaySignalingMode",
        message: 'l2RelaySignalingMode must be "ws-trickle", "http-offer", or "legacy-offer".',
      });
    }
  }

  if (hasOwn(input, "l2TunnelToken")) {
    const parsed = parseNonEmptyString(input.l2TunnelToken);
    if (parsed !== undefined) {
      overrides.l2TunnelToken = parsed;
    }
  }

  if (hasOwn(input, "l2TunnelTokenTransport")) {
    const parsed = parseL2TunnelTokenTransport(input.l2TunnelTokenTransport);
    if (parsed !== undefined) {
      overrides.l2TunnelTokenTransport = parsed;
    } else {
      issues.push({
        key: "l2TunnelTokenTransport",
        message: 'l2TunnelTokenTransport must be "query", "subprotocol", or "both".',
      });
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

  if (hasOwn(input, "vmRuntime")) {
    const parsed = parseVmRuntime(input.vmRuntime);
    if (parsed !== undefined) {
      overrides.vmRuntime = parsed;
    } else {
      issues.push({ key: "vmRuntime", message: 'vmRuntime must be "legacy" or "machine".' });
    }
  }

  if (hasOwn(input, "uiScale")) {
    const parsed = parseUiScale(input.uiScale);
    if (parsed) {
      overrides.uiScale = parsed.value;
      if ("issue" in parsed) issues.push({ key: "uiScale", message: parsed.issue });
    }
  }

  if (hasOwn(input, "forceKeyboardBackend")) {
    const parsed = parseInputBackendOverride(input.forceKeyboardBackend);
    if (parsed !== undefined) {
      overrides.forceKeyboardBackend = parsed;
    } else {
      issues.push({
        key: "forceKeyboardBackend",
        message: 'forceKeyboardBackend must be "auto", "ps2", "usb", or "virtio".',
      });
    }
  }

  if (hasOwn(input, "forceMouseBackend")) {
    const parsed = parseInputBackendOverride(input.forceMouseBackend);
    if (parsed !== undefined) {
      overrides.forceMouseBackend = parsed;
    } else {
      issues.push({
        key: "forceMouseBackend",
        message: 'forceMouseBackend must be "auto", "ps2", "usb", or "virtio".',
      });
    }
  }

  if (hasOwn(input, "virtioNetMode")) {
    const parsed = parseVirtioPciMode(input.virtioNetMode);
    if (parsed !== undefined) {
      overrides.virtioNetMode = parsed;
    } else {
      issues.push({ key: "virtioNetMode", message: 'virtioNetMode must be "modern", "transitional", or "legacy".' });
    }
  }

  if (hasOwn(input, "virtioInputMode")) {
    const parsed = parseVirtioPciMode(input.virtioInputMode);
    if (parsed !== undefined) {
      overrides.virtioInputMode = parsed;
    } else {
      issues.push({
        key: "virtioInputMode",
        message: 'virtioInputMode must be "modern", "transitional", or "legacy".',
      });
    }
  }

  if (hasOwn(input, "virtioSndMode")) {
    const parsed = parseVirtioPciMode(input.virtioSndMode);
    if (parsed !== undefined) {
      overrides.virtioSndMode = parsed;
    } else {
      issues.push({ key: "virtioSndMode", message: 'virtioSndMode must be "modern", "transitional", or "legacy".' });
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

  // Option C networking (L2 tunnel) query overrides:
  // - `l2=ws|webrtc`
  // - `l2Signal=ws-trickle|http-offer|legacy-offer`
  // - `l2Token=<token>` (optional)
  // - `l2TokenTransport=query|subprotocol|both` (optional)
  const l2 = params.get("l2");
  if (l2 !== null) {
    const parsed = parseL2TunnelTransport(l2);
    if (parsed !== undefined) {
      overrides.l2TunnelTransport = parsed;
      lockedKeys.add("l2TunnelTransport");
    } else {
      issues.push({ key: "l2TunnelTransport", message: 'l2 must be "ws" or "webrtc".' });
    }
  }

  const l2Signal = params.get("l2Signal");
  if (l2Signal !== null) {
    const parsed = parseRelaySignalingMode(l2Signal);
    if (parsed !== undefined) {
      overrides.l2RelaySignalingMode = parsed;
      lockedKeys.add("l2RelaySignalingMode");
    } else {
      issues.push({
        key: "l2RelaySignalingMode",
        message: 'l2Signal must be "ws-trickle", "http-offer", or "legacy-offer".',
      });
    }
  }

  const l2Token = params.get("l2Token");
  if (l2Token !== null) {
    const parsed = parseNonEmptyString(l2Token);
    if (parsed !== undefined) {
      overrides.l2TunnelToken = parsed;
      lockedKeys.add("l2TunnelToken");
    }
  }

  const l2TokenTransport = params.get("l2TokenTransport");
  if (l2TokenTransport !== null) {
    const parsed = parseL2TunnelTokenTransport(l2TokenTransport);
    if (parsed !== undefined) {
      overrides.l2TunnelTokenTransport = parsed;
      lockedKeys.add("l2TunnelTokenTransport");
    } else {
      issues.push({
        key: "l2TunnelTokenTransport",
        message: 'l2TokenTransport must be "query", "subprotocol", or "both".',
      });
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

  const vmRuntime = params.get("vmRuntime");
  if (vmRuntime !== null) {
    const parsed = parseVmRuntime(vmRuntime);
    if (parsed !== undefined) {
      overrides.vmRuntime = parsed;
      lockedKeys.add("vmRuntime");
    } else {
      issues.push({ key: "vmRuntime", message: 'vmRuntime must be "legacy" or "machine".' });
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

  const vm = params.get("vm");
  if (vm !== null) {
    const parsed = parseVmRuntime(vm);
    if (parsed !== undefined) {
      overrides.vmRuntime = parsed;
      lockedKeys.add("vmRuntime");
    } else {
      issues.push({ key: "vmRuntime", message: 'vm must be "legacy" or "machine".' });
    }
  } else {
    // Backwards/short-hand query param: ?machine=1
    const machine = params.get("machine");
    if (machine !== null) {
      const parsed = parseBoolean(machine);
      if (parsed !== undefined) {
        overrides.vmRuntime = parsed ? "machine" : "legacy";
        lockedKeys.add("vmRuntime");
      } else {
        issues.push({ key: "vmRuntime", message: 'machine must be a boolean ("1"/"0"/"true"/"false").' });
      }
    }
  }

  const kbd = params.get("kbd") ?? params.get("keyboard");
  if (kbd !== null) {
    const parsed = parseInputBackendOverride(kbd);
    if (parsed !== undefined) {
      overrides.forceKeyboardBackend = parsed;
      lockedKeys.add("forceKeyboardBackend");
    } else {
      issues.push({
        key: "forceKeyboardBackend",
        message: 'kbd must be "auto", "ps2", "usb", or "virtio".',
      });
    }
  }

  const mouse = params.get("mouse");
  if (mouse !== null) {
    const parsed = parseInputBackendOverride(mouse);
    if (parsed !== undefined) {
      overrides.forceMouseBackend = parsed;
      lockedKeys.add("forceMouseBackend");
    } else {
      issues.push({
        key: "forceMouseBackend",
        message: 'mouse must be "auto", "ps2", "usb", or "virtio".',
      });
    }
  }

  const virtioNetMode = params.get("virtioNetMode");
  if (virtioNetMode !== null) {
    const parsed = parseVirtioPciMode(virtioNetMode);
    if (parsed !== undefined) {
      overrides.virtioNetMode = parsed;
      lockedKeys.add("virtioNetMode");
    } else {
      issues.push({ key: "virtioNetMode", message: 'virtioNetMode must be "modern", "transitional", or "legacy".' });
    }
  }

  const virtioInputMode = params.get("virtioInputMode");
  if (virtioInputMode !== null) {
    const parsed = parseVirtioPciMode(virtioInputMode);
    if (parsed !== undefined) {
      overrides.virtioInputMode = parsed;
      lockedKeys.add("virtioInputMode");
    } else {
      issues.push({
        key: "virtioInputMode",
        message: 'virtioInputMode must be "modern", "transitional", or "legacy".',
      });
    }
  }

  const virtioSndMode = params.get("virtioSndMode");
  if (virtioSndMode !== null) {
    const parsed = parseVirtioPciMode(virtioSndMode);
    if (parsed !== undefined) {
      overrides.virtioSndMode = parsed;
      lockedKeys.add("virtioSndMode");
    } else {
      issues.push({ key: "virtioSndMode", message: 'virtioSndMode must be "modern", "transitional", or "legacy".' });
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
