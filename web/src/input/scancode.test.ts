import { describe, expect, it } from "vitest";

import { shouldPreventDefaultForKeyboardEvent } from "./scancode";

function makeEvent(opts: { code: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean }): KeyboardEvent {
  return {
    code: opts.code,
    ctrlKey: opts.ctrlKey ?? false,
    metaKey: opts.metaKey ?? false,
    altKey: opts.altKey ?? false,
  } as unknown as KeyboardEvent;
}

describe("shouldPreventDefaultForKeyboardEvent", () => {
  it("prevents default for navigation keys + function keys unless Ctrl/Meta is held", () => {
    const codes = [
      "Escape",
      "ArrowUp",
      "ArrowDown",
      "ArrowLeft",
      "ArrowRight",
      "Space",
      "PageUp",
      "PageDown",
      "Home",
      "End",
      "Tab",
      "Backspace",
      "AltLeft",
      "AltRight",
      "ContextMenu",
      "BrowserBack",
      "BrowserForward",
      "BrowserRefresh",
      "BrowserStop",
      "BrowserHome",
      "F1",
      "F2",
      "F3",
      "F4",
      "F5",
      "F6",
      "F7",
      "F8",
      "F9",
      "F10",
      "F11",
      "F12",
    ];

    for (const code of codes) {
      expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code }))).toBe(true);
      expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code, ctrlKey: true }))).toBe(false);
      expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code, metaKey: true }))).toBe(false);
    }
  });

  it("prevents default for Alt-modified keystrokes by default", () => {
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "KeyD", altKey: true }))).toBe(true);
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "Digit4", altKey: true }))).toBe(true);

    // Ctrl/Meta are treated as "host shortcut" modifiers and should override the Alt rule.
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "KeyD", altKey: true, ctrlKey: true }))).toBe(false);
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "KeyD", altKey: true, metaKey: true }))).toBe(false);
  });

  it("does not prevent default for ordinary keys by default", () => {
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "KeyA" }))).toBe(false);
    expect(shouldPreventDefaultForKeyboardEvent(makeEvent({ code: "Enter" }))).toBe(false);
  });
});
