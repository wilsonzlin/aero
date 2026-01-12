import { describe, expect, it } from "vitest";

import { chooseKeyboardInputBackend, chooseMouseInputBackend } from "./input_backend_selection";

describe("input_backend_selection", () => {
  describe("chooseKeyboardInputBackend", () => {
    it("does not switch while keys are held", () => {
      expect(
        chooseKeyboardInputBackend({
          current: "ps2",
          keysHeld: true,
          virtioOk: true,
          usbOk: true,
        }),
      ).toBe("ps2");

      expect(
        chooseKeyboardInputBackend({
          current: "usb",
          keysHeld: true,
          virtioOk: false,
          usbOk: false,
        }),
      ).toBe("usb");
    });

    it("prefers virtio when available and no keys are held", () => {
      expect(
        chooseKeyboardInputBackend({
          current: "ps2",
          keysHeld: false,
          virtioOk: true,
          usbOk: true,
        }),
      ).toBe("virtio");
    });

    it("falls back to usb when virtio is unavailable and usb is ok", () => {
      expect(
        chooseKeyboardInputBackend({
          current: "ps2",
          keysHeld: false,
          virtioOk: false,
          usbOk: true,
        }),
      ).toBe("usb");
    });

    it("falls back to ps2 when neither virtio nor usb is available", () => {
      expect(
        chooseKeyboardInputBackend({
          current: "usb",
          keysHeld: false,
          virtioOk: false,
          usbOk: false,
        }),
      ).toBe("ps2");
    });
  });

  describe("chooseMouseInputBackend", () => {
    it("does not switch while buttons are held", () => {
      expect(
        chooseMouseInputBackend({
          current: "ps2",
          buttonsHeld: true,
          virtioOk: true,
          usbOk: true,
        }),
      ).toBe("ps2");

      expect(
        chooseMouseInputBackend({
          current: "virtio",
          buttonsHeld: true,
          virtioOk: false,
          usbOk: false,
        }),
      ).toBe("virtio");
    });

    it("prefers virtio when available and no buttons are held", () => {
      expect(
        chooseMouseInputBackend({
          current: "ps2",
          buttonsHeld: false,
          virtioOk: true,
          usbOk: true,
        }),
      ).toBe("virtio");
    });

    it("falls back to usb when virtio is unavailable and usb is ok", () => {
      expect(
        chooseMouseInputBackend({
          current: "ps2",
          buttonsHeld: false,
          virtioOk: false,
          usbOk: true,
        }),
      ).toBe("usb");
    });

    it("falls back to ps2 when neither virtio nor usb is available", () => {
      expect(
        chooseMouseInputBackend({
          current: "usb",
          buttonsHeld: false,
          virtioOk: false,
          usbOk: false,
        }),
      ).toBe("ps2");
    });
  });
});

