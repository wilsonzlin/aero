import { formatOneLineError, formatOneLineUtf8 } from "../text";

export type WebUsbErrorExplanation = {
  title: string;
  details?: string;
  hints: string[];
};

type ErrorLike = {
  name?: unknown;
  message?: unknown;
  cause?: unknown;
};

type HostOs = "windows" | "linux" | "mac" | "android" | "unknown";

const MAX_WEBUSB_ERROR_BYTES = 512;

function safeToString(value: unknown): string {
  return formatOneLineError(value, MAX_WEBUSB_ERROR_BYTES);
}

function normalizeError(err: unknown): { name?: string; message?: string } {
  if (typeof err === "string") {
    return { message: err };
  }

  if (typeof err === "object" && err !== null) {
    let name: unknown;
    let message: unknown;
    try {
      name = (err as ErrorLike).name;
    } catch {
      name = undefined;
    }
    try {
      message = (err as ErrorLike).message;
    } catch {
      message = undefined;
    }
    return {
      name: typeof name === "string" ? name : undefined,
      message: typeof message === "string" ? message : undefined,
    };
  }

  return {};
}

function normalizeErrorChain(err: unknown, maxDepth = 5): Array<{ name?: string; message?: string }> {
  const chain: Array<{ name?: string; message?: string }> = [];
  const seen = new Set<unknown>();

  const parseStringChain = (text: string): Array<{ name?: string; message?: string }> | null => {
    const parts = text
      .split("<-")
      .map((p) => p.trim())
      .filter((p) => p.length > 0);
    if (parts.length === 1) {
      const match = /^([A-Za-z]+Error):\s*(.*)$/.exec(parts[0]);
      if (!match) return null;
      const [, parsedName, parsedMessage] = match;
      // Ignore the generic "Error: ..." prefix, but accept DOMException-style names
      // like "NetworkError: ...".
      if (parsedName === "Error") return null;
      return [{ name: parsedName, message: parsedMessage || undefined }];
    }
    if (parts.length <= 1) return null;
    return parts.map((part) => {
      const match = /^([A-Za-z]+Error):\s*(.*)$/.exec(part);
      if (match) {
        const [, parsedName, parsedMessage] = match;
        return { name: parsedName, message: parsedMessage || undefined };
      }
      return { message: part };
    });
  };

  let cur: unknown = err;
  for (let depth = 0; depth < maxDepth; depth += 1) {
    if (typeof cur === "string") {
      const parsed = parseStringChain(cur);
      if (parsed) chain.push(...parsed);
      else chain.push({ message: cur });
      break;
    }

    if (typeof cur === "object" && cur !== null) {
      if (seen.has(cur)) break;
      seen.add(cur);

      let name: unknown;
      let message: unknown;
      let cause: unknown;
      try {
        name = (cur as ErrorLike).name;
      } catch {
        name = undefined;
      }
      try {
        message = (cur as ErrorLike).message;
      } catch {
        message = undefined;
      }
      try {
        cause = (cur as ErrorLike).cause;
      } catch {
        cause = undefined;
      }

      const parsedName = typeof name === "string" ? name : undefined;
      const parsedMessage = typeof message === "string" ? message : undefined;

      // If we have a single string containing a formatted chain (e.g.
      // `Error: ... <- NetworkError: ...`), parse it back into a structured list
      // so callers can still decode the underlying DOMException name.
      if (parsedMessage && typeof cause === "undefined") {
        const parsed = parseStringChain(parsedMessage);
        if (parsed) {
          chain.push(...parsed);
          break;
        }
      }

      chain.push({ name: parsedName, message: parsedMessage });

      if (cause === undefined) break;
      cur = cause;
      continue;
    }

    if (cur !== undefined) {
      chain.push({ message: safeToString(cur) });
    }
    break;
  }

  return chain;
}

function includesAny(haystack: string, needles: string[]): boolean {
  return needles.some((needle) => haystack.includes(needle));
}

function detectHostOs(): HostOs {
  if (typeof navigator === "undefined") {
    return "unknown";
  }

  const navMaybeUaData = navigator as Navigator & { userAgentData?: { platform?: unknown } };
  const uaDataPlatform = navMaybeUaData.userAgentData?.platform;
  if (typeof uaDataPlatform === "string") {
    const platform = uaDataPlatform.toLowerCase();
    if (platform.includes("windows")) return "windows";
    if (platform.includes("mac") || platform.includes("ios")) return "mac";
    if (platform.includes("android")) return "android";
    if (platform.includes("linux")) return "linux";
  }

  if (typeof navigator.userAgent !== "string") {
    return "unknown";
  }

  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows")) return "windows";
  if (ua.includes("mac os") || ua.includes("macintosh")) return "mac";
  if (ua.includes("android")) return "android";
  if (ua.includes("linux")) return "linux";
  return "unknown";
}

function permissionsPolicyAllowsUsb(): boolean | null {
  if (typeof document === "undefined") return null;
  const doc = document as unknown as {
    permissionsPolicy?: { allowsFeature?: (feature: string) => boolean };
    featurePolicy?: { allowsFeature?: (feature: string) => boolean };
  };
  const policy = doc.permissionsPolicy ?? doc.featurePolicy;
  if (!policy || typeof policy.allowsFeature !== "function") return null;
  try {
    return policy.allowsFeature("usb");
  } catch {
    return null;
  }
}

/**
 * Decode a WebUSB error into actionable troubleshooting hints.
 *
 * This is intentionally best-effort: browsers surface WebUSB failures as
 * `DOMException`s with inconsistent (and frequently opaque) messages.
 *
 * The helper is:
 * - defensive (never throws),
 * - stable (matches on broad substrings rather than exact Chromium copy),
 * - user-facing (actionable hints, including OS-specific driver/permission notes).
 */
export function explainWebUsbError(err: unknown): WebUsbErrorExplanation {
  const chain = normalizeErrorChain(err);
  const primary =
    chain.find((entry) => entry.name && entry.name !== "Error") ?? chain.find((entry) => entry.name) ?? chain[0] ?? {};
  const name = primary.name;
  const msg = chain
    .map((entry) => entry.message)
    .filter((m): m is string => typeof m === "string" && m.length > 0)
    .join(" | ");
  const msgLower = msg.toLowerCase();
  const hostOs = detectHostOs();

  const hints: string[] = [];
  const seen = new Set<string>();
  const addHint = (hint: string): void => {
    if (seen.has(hint)) return;
    seen.add(hint);
    hints.push(hint);
  };

  const ppUsb = permissionsPolicyAllowsUsb();
  const ppBlocksUsb = ppUsb === false;

  const secureContextValue =
    typeof (globalThis as typeof globalThis & { isSecureContext?: unknown }).isSecureContext === "boolean"
      ? (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext
      : null;
  const insecureContext = secureContextValue === false;
  const mentionsSecureContext = includesAny(msgLower, ["secure context", "secure origin", "only secure"]);

  const mentionsUserGesture = includesAny(msgLower, ["user gesture", "user activation"]);
  const mentionsPermissionDenied = includesAny(msgLower, ["permission", "not allowed", "denied", "access denied"]);
  const mentionsFilters = includesAny(msgLower, ["filter", "filters"]);
  const mentionsPermissionsPolicy = includesAny(msgLower, ["permissions policy", "permission policy", "feature policy"]);
  const mentionsAbort = includesAny(msgLower, ["abort", "aborted", "aborterror", "cancelled", "canceled"]);
  const mentionsIsochronous = includesAny(msgLower, ["isochronous"]);
  const mentionsClone = includesAny(msgLower, ["datacloneerror", "could not be cloned", "data clone", "structured clone"]);
  const mentionsEndpoint = includesAny(msgLower, ["endpoint"]);
  const mentionsClaimInterface = includesAny(msgLower, ["claiminterface", "claim interface", "unable to claim"]);
  const mentionsOpen = includesAny(msgLower, ["failed to open", "unable to open", "open()"]);
  const mentionsDisconnected = includesAny(msgLower, ["disconnected", "not found", "no device selected"]);
  const mentionsProtectedInterface = includesAny(msgLower, [
    "protected interface",
    "protected class",
    "is protected",
    "interface is protected",
    "webusb-protected",
    "no claimable interface",
  ]);

  const driverOrPermissionsLikely =
    !mentionsProtectedInterface &&
    (mentionsClaimInterface ||
      mentionsOpen ||
      (mentionsPermissionDenied && includesAny(msgLower, ["claim", "open"])) ||
      name === "NotReadableError" ||
      name === "NetworkError");

  // --- Titles (short, user-facing) ---
  let title = "WebUSB operation failed";
  let details: string | undefined;

  switch (name) {
    case "TypeError":
      title = "WebUSB call failed (invalid parameters or unsupported usage)";
      details = "The browser rejected the WebUSB call; this is often caused by `requestDevice()` filter requirements.";
      break;
    case "NotFoundError":
      title = "No USB device selected (or the device was disconnected)";
      details = "The browser did not have an active permission grant for a device.";
      break;
    case "DataCloneError":
      title = "WebUSB device handle cannot be transferred to a worker";
      details = "The browser failed to structured-clone a USBDevice (or related handle) across threads.";
      break;
    case "AbortError":
      title = "WebUSB operation was aborted";
      details = "The operation was canceled (for example, the chooser was closed) or interrupted.";
      break;
    case "NotSupportedError":
      title = "WebUSB operation is not supported";
      details = "The browser does not support this operation for the selected device/interface.";
      break;
    case "InvalidAccessError":
      title = "WebUSB request was rejected";
      details = "The requested device/interface/endpoint could not be accessed in the current context.";
      break;
    case "OperationError":
      title = "WebUSB device operation failed";
      details = "The device did not complete the requested operation (transfer/open/claim).";
      break;
    case "NotReadableError":
      title = "Unable to access the USB device";
      details = "The device may be busy, already opened by another process, or blocked by OS driver/permissions.";
      break;
    case "NotAllowedError":
      title = "WebUSB permission was denied";
      details = "The browser blocked the request (often due to user gesture or permission policy requirements).";
      break;
    case "SecurityError":
      title = "WebUSB access was blocked by browser security restrictions";
      details = "This often indicates an insecure context or a protected USB interface class.";
      break;
    case "InvalidStateError":
      title = "WebUSB device is in the wrong state";
      details = "This usually means the device is not opened/configured yet, or the interface is already claimed.";
      break;
    case "NetworkError":
      title = "WebUSB communication failed";
      details = "This often indicates a driver/permissions issue or the device becoming unavailable.";
      break;
    default:
      if (mentionsUserGesture) {
        title = "User gesture required for WebUSB";
      } else if (mentionsSecureContext) {
        title = "Secure context required for WebUSB";
      } else if (mentionsClone) {
        title = "WebUSB device handle cannot be transferred to a worker";
      } else if (mentionsClaimInterface) {
        title = "Unable to claim the USB interface";
      } else if (mentionsOpen) {
        title = "Unable to open the USB device";
      } else if (mentionsDisconnected) {
        title = "USB device not found (or disconnected)";
      } else if (mentionsAbort) {
        title = "WebUSB operation was aborted";
      } else if (mentionsIsochronous) {
        title = "WebUSB operation is not supported for isochronous endpoints";
      } else if (mentionsEndpoint) {
        title = "WebUSB endpoint access failed";
      }
      break;
  }

  // --- Hints ---
  if (
    insecureContext ||
    mentionsSecureContext ||
    (name === "SecurityError" && secureContextValue !== true && !mentionsProtectedInterface)
  ) {
    addHint("WebUSB requires a secure context: use https:// or http://localhost (check `isSecureContext`).");
  }

  if (name === "NotAllowedError" || mentionsUserGesture) {
    addHint("`navigator.usb.requestDevice()` must be called from a user gesture (e.g. a button click).");
    addHint(
      "If you `await` before calling `requestDevice()`, the user gesture can be lost; call it directly in the click handler.",
    );
    addHint(
      "If you denied the permission prompt previously, remove the USB permission in your browser's site settings and try again.",
    );
  }

  if (name === "TypeError" && mentionsFilters) {
    addHint(
      "`requestDevice()` filtering rules vary by Chromium version. If the browser rejects your options, try providing an explicit `vendorId`/`productId` filter (or `acceptAllDevices: true` if supported).",
    );
  }

  if (ppBlocksUsb) {
    addHint("This document's Permissions Policy blocks WebUSB (`document.permissionsPolicy.allowsFeature('usb')` is false).");
  }
  if (mentionsPermissionsPolicy || ppBlocksUsb) {
    addHint(
      "WebUSB can be blocked by Permissions Policy / enterprise policy. If you're running in an iframe, ensure the frame is allowed to use WebUSB (e.g. `allow=\"usb\"`) and that response headers permit it.",
    );
  }

  if (name === "InvalidStateError") {
    addHint("Call `await device.open()` before `claimInterface()` / transfers.");
    addHint(
      "If `device.configuration` is null, call `await device.selectConfiguration(1)` (or the appropriate configuration) before `claimInterface()`.",
    );
    addHint("Close/release the device when done (`releaseInterface`, `close`) and avoid double-claiming interfaces.");
  }

  if (name === "DataCloneError" || mentionsClone) {
    addHint(
      "Some browsers do not support structured-cloning `USBDevice` objects. Keep WebUSB calls on the same thread (typically the main thread) and proxy I/O to workers instead of sending the device handle.",
    );
    addHint(
      "If `WorkerNavigator.usb` is available, a worker can call `navigator.usb.getDevices()` to obtain permitted devices after the main thread has granted permission via `requestDevice()`.",
    );
  }

  if (name === "InvalidAccessError") {
    addHint("Ensure the device is opened, a configuration is selected, and the interface is claimed before transfers.");
    addHint("Double-check endpoint numbers and directions (IN/OUT) against the device descriptors.");
    addHint("If the interface uses alternate settings, select the correct alternate setting before transfers.");
  }

  if (name === "NotSupportedError" || mentionsIsochronous) {
    addHint(
      "WebUSB typically supports control/bulk/interrupt transfers only; isochronous endpoints (common for USB audio/video) are not generally supported.",
    );
  }

  if (name === "OperationError") {
    addHint("Try unplug/replug the device and retry the operation.");
    addHint("If supported, try resetting the device (`device.reset()`) or closing/re-opening it.");
  }

  if (name === "NotFoundError" || name === "AbortError" || mentionsDisconnected) {
    addHint("If you canceled the browser chooser, just run `requestDevice()` again.");
    addHint("Ensure the device is plugged in, then run `requestDevice()` again and re-select it in the chooser.");
    addHint("If the chooser is empty, check your `filters` (vendorId/productId) and that the device is not in use.");
    addHint(
      "If the chooser doesn't show your device at all, it may expose only \"protected\" interface classes (HID, mass storage, audio/video, etc.) and be hidden/blocked by Chromium's WebUSB restrictions.",
    );
  }

  if (name === "SecurityError" || mentionsProtectedInterface) {
    addHint(
      "Some USB interface classes are blocked by WebUSB (\"protected\" classes like HID, mass storage, audio/video, etc.). Use a vendor-specific interface (class 0xFF) or a more appropriate web API (e.g. WebHID/WebSerial).",
    );
  }

  if (driverOrPermissionsLikely) {
    addHint("Close other applications that may be using the device, then unplug/replug it and try again.");
    addHint(
      "In Chrome/Edge, you can inspect WebUSB state at chrome://usb-internals (or edge://usb-internals) to see detected devices, claimed interfaces, and recent errors.",
    );

    if (hostOs === "windows" || hostOs === "unknown") {
      addHint(
        "Windows: the interface usually needs to be bound to WinUSB for WebUSB to work. Install WinUSB (e.g. via Zadig) or ship Microsoft OS 2.0 descriptors / WinUSB compatible ID descriptors so Windows picks WinUSB automatically.",
      );
    }

    if (hostOs === "linux" || hostOs === "unknown") {
      addHint(
        "Linux: ensure your user has device permissions (udev rules). Also ensure no kernel driver is attached to the interface (a bound kernel driver can prevent `claimInterface()`).",
      );
    }

    if (hostOs === "mac") {
      addHint(
        "macOS: if the OS has a built-in driver attached to the interface, WebUSB may not be able to claim it. Vendor-specific interfaces (class 0xFF) are the most feasible.",
      );
    }

    if (hostOs === "android") {
      addHint(
        "Android: WebUSB support and USB permissions vary by device/OTG support. If possible, try a desktop Chromium browser for troubleshooting.",
      );
    }
  }

  if (hints.length === 0) {
    addHint("Check the browser console for the raw error message and confirm the device is still connected.");
    addHint("If this is `requestDevice()`, ensure it's called from a user gesture and you're in a secure context.");
  }

  return { title, details, hints };
}

export function formatWebUsbError(err: unknown): string {
  const chain = normalizeErrorChain(err);
  const formatted = chain
    .map(({ name, message }) => {
      if (name && message) return `${name}: ${message}`;
      if (name) return name;
      if (message) return message;
      return null;
    })
    .filter((part): part is string => !!part);
  if (formatted.length === 0) return safeToString(err);
  return formatOneLineUtf8(formatted.join(" <- "), MAX_WEBUSB_ERROR_BYTES) || "Error";
}
