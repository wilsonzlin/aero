import type { WebUsbBackendOptions } from "./webusb_backend";
import type { UsbBrokerPortLike } from "./webusb_passthrough_runtime";
import { unrefBestEffort } from "../unrefSafe";

/**
 * Create a dedicated {@link MessagePort} attached to the main-thread {@link UsbBroker}.
 *
 * The broker routes actions based on the port they arrive on, so using a subport allows callers
 * to customize per-port backend options (e.g. disabling the UHCI-only
 * CONFIGURATION→OTHER_SPEED_CONFIGURATION descriptor translation for high-speed controllers).
 *
 * If `MessageChannel` is unavailable (older environments), falls back to `parent`.
 */
export function createUsbBrokerSubport<TParent extends UsbBrokerPortLike>(
  parent: TParent,
  options: { attachRings?: boolean; backendOptions?: WebUsbBackendOptions } = {},
): MessagePort | TParent {
  if (typeof MessageChannel === "undefined") return parent;

  try {
    const channel = new MessageChannel();
    parent.postMessage(
      {
        type: "usb.broker.attachPort",
        port: channel.port2,
        attachRings: options.attachRings,
        backendOptions: options.backendOptions,
      },
      [channel.port2],
    );

    try {
      // Node/Vitest may keep MessagePorts alive; unref so unit tests don't hang.
      unrefBestEffort(channel.port1);
      unrefBestEffort(channel.port2);
    } catch {
      // ignore
    }

    return channel.port1;
  } catch {
    // Fall back to the default worker channel when MessageChannel is unavailable or cannot be used.
    return parent;
  }
}

export function createUsbBrokerSubportNoOtherSpeedTranslation<TParent extends UsbBrokerPortLike>(
  parent: TParent,
): MessagePort | TParent {
  // Avoid the attachRings race: the port is attached on the broker side before the caller's
  // runtime adds its own `message` event listener. Instead, let the runtime send
  // `usb.ringAttachRequest` after it is ready.
  return createUsbBrokerSubport(parent, {
    attachRings: false,
    backendOptions: { translateOtherSpeedConfigurationDescriptor: false },
  });
}

