export type WebUsbErrorExplanation = {
  title: string;
  details?: string;
  hints: string[];
};

type ErrorLike = {
  name?: unknown;
  message?: unknown;
};

type HostOs = "windows" | "linux" | "mac" | "unknown";

function safeToString(value: unknown): string {
  try {
    return String(value);
  } catch {
    return "[unprintable]";
  }
}

function normalizeError(err: unknown): { name?: string; message?: string } {
  if (typeof err === "string") {
    return { message: err };
  }

  if (typeof err === "object" && err !== null) {
    const { name, message } = err as ErrorLike;
    return {
      name: typeof name === "string" ? name : undefined,
      message: typeof message === "string" ? message : undefined,
    };
  }

  return {};
}

function includesAny(haystack: string, needles: string[]): boolean {
  return needles.some((needle) => haystack.includes(needle));
}

function detectHostOs(): HostOs {
  if (typeof navigator === "undefined" || typeof navigator.userAgent !== "string") {
    return "unknown";
  }

  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("windows")) return "windows";
  if (ua.includes("mac os") || ua.includes("macintosh")) return "mac";
  // Android user agents contain "Linux"; treat Android separately and fall back
  // to generic advice elsewhere.
  if (ua.includes("android")) return "unknown";
  if (ua.includes("linux")) return "linux";
  return "unknown";
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
  const { name, message } = normalizeError(err);
  const msg = message ?? "";
  const msgLower = msg.toLowerCase();
  const hostOs = detectHostOs();

  const hints: string[] = [];
  const seen = new Set<string>();
  const addHint = (hint: string): void => {
    if (seen.has(hint)) return;
    seen.add(hint);
    hints.push(hint);
  };

  const insecureContext =
    typeof (globalThis as typeof globalThis & { isSecureContext?: unknown }).isSecureContext === "boolean" &&
    (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext === false;
  const mentionsSecureContext = includesAny(msgLower, ["secure context", "secure origin", "only secure"]);

  const mentionsUserGesture = includesAny(msgLower, ["user gesture", "user activation"]);
  const mentionsPermissionDenied = includesAny(msgLower, ["permission", "not allowed", "denied", "access denied"]);
  const mentionsFilters = includesAny(msgLower, ["filter", "filters"]);
  const mentionsPermissionsPolicy = includesAny(msgLower, ["permissions policy", "permission policy", "feature policy"]);
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
    mentionsClaimInterface ||
    mentionsOpen ||
    (mentionsPermissionDenied && includesAny(msgLower, ["claim", "interface", "open"])) ||
    name === "NetworkError";

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
      } else if (mentionsClaimInterface) {
        title = "Unable to claim the USB interface";
      } else if (mentionsOpen) {
        title = "Unable to open the USB device";
      } else if (mentionsDisconnected) {
        title = "USB device not found (or disconnected)";
      }
      break;
  }

  // --- Hints ---
  if (insecureContext || mentionsSecureContext || name === "SecurityError") {
    addHint("WebUSB requires a secure context: use https:// or http://localhost (check `isSecureContext`).");
  }

  if (name === "NotAllowedError" || mentionsUserGesture) {
    addHint("`navigator.usb.requestDevice()` must be called from a user gesture (e.g. a button click).");
    addHint(
      "If you `await` before calling `requestDevice()`, the user gesture can be lost; call it directly in the click handler.",
    );
  }

  if (name === "TypeError" && mentionsFilters) {
    addHint(
      "`requestDevice()` filtering rules vary by Chromium version. If the browser rejects your options, try providing an explicit `vendorId`/`productId` filter (or `acceptAllDevices: true` if supported).",
    );
  }

  if (mentionsPermissionsPolicy) {
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

  if (name === "NotFoundError" || mentionsDisconnected) {
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
  }

  if (hints.length === 0) {
    addHint("Check the browser console for the raw error message and confirm the device is still connected.");
    addHint("If this is `requestDevice()`, ensure it's called from a user gesture and you're in a secure context.");
  }

  return { title, details, hints };
}

export function formatWebUsbError(err: unknown): string {
  const { name, message } = normalizeError(err);
  if (name && message) return `${name}: ${message}`;
  if (name) return name;
  if (message) return message;
  return safeToString(err);
}
