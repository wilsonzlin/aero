export type Set2Scancode = {
  code: number;
  extended: boolean;
};

// JavaScript `KeyboardEvent.code` → PS/2 Set 2 make code mapping.
//
// Notes:
// - For extended keys, PS/2 prepends 0xE0.
// - Break code is 0xF0 + make code (and for extended, 0xE0 0xF0 ...).
//
// Keep this table small and extend it as guest software requires more keys.
const SET2_BY_CODE: Record<string, Set2Scancode> = {
  Escape: { code: 0x76, extended: false },
  F1: { code: 0x05, extended: false },
  F2: { code: 0x06, extended: false },
  F3: { code: 0x04, extended: false },
  F4: { code: 0x0c, extended: false },
  F5: { code: 0x03, extended: false },
  F6: { code: 0x0b, extended: false },
  F7: { code: 0x83, extended: false },
  F8: { code: 0x0a, extended: false },
  F9: { code: 0x01, extended: false },
  F10: { code: 0x09, extended: false },
  F11: { code: 0x78, extended: false },
  F12: { code: 0x07, extended: false },

  Backquote: { code: 0x0e, extended: false },
  Digit1: { code: 0x16, extended: false },
  Digit2: { code: 0x1e, extended: false },
  Digit3: { code: 0x26, extended: false },
  Digit4: { code: 0x25, extended: false },
  Digit5: { code: 0x2e, extended: false },
  Digit6: { code: 0x36, extended: false },
  Digit7: { code: 0x3d, extended: false },
  Digit8: { code: 0x3e, extended: false },
  Digit9: { code: 0x46, extended: false },
  Digit0: { code: 0x45, extended: false },
  Minus: { code: 0x4e, extended: false },
  Equal: { code: 0x55, extended: false },
  Backspace: { code: 0x66, extended: false },

  Tab: { code: 0x0d, extended: false },
  KeyQ: { code: 0x15, extended: false },
  KeyW: { code: 0x1d, extended: false },
  KeyE: { code: 0x24, extended: false },
  KeyR: { code: 0x2d, extended: false },
  KeyT: { code: 0x2c, extended: false },
  KeyY: { code: 0x35, extended: false },
  KeyU: { code: 0x3c, extended: false },
  KeyI: { code: 0x43, extended: false },
  KeyO: { code: 0x44, extended: false },
  KeyP: { code: 0x4d, extended: false },
  BracketLeft: { code: 0x54, extended: false },
  BracketRight: { code: 0x5b, extended: false },
  Backslash: { code: 0x5d, extended: false },

  CapsLock: { code: 0x58, extended: false },
  KeyA: { code: 0x1c, extended: false },
  KeyS: { code: 0x1b, extended: false },
  KeyD: { code: 0x23, extended: false },
  KeyF: { code: 0x2b, extended: false },
  KeyG: { code: 0x34, extended: false },
  KeyH: { code: 0x33, extended: false },
  KeyJ: { code: 0x3b, extended: false },
  KeyK: { code: 0x42, extended: false },
  KeyL: { code: 0x4b, extended: false },
  Semicolon: { code: 0x4c, extended: false },
  Quote: { code: 0x52, extended: false },
  Enter: { code: 0x5a, extended: false },

  ShiftLeft: { code: 0x12, extended: false },
  KeyZ: { code: 0x1a, extended: false },
  KeyX: { code: 0x22, extended: false },
  KeyC: { code: 0x21, extended: false },
  KeyV: { code: 0x2a, extended: false },
  KeyB: { code: 0x32, extended: false },
  KeyN: { code: 0x31, extended: false },
  KeyM: { code: 0x3a, extended: false },
  Comma: { code: 0x41, extended: false },
  Period: { code: 0x49, extended: false },
  Slash: { code: 0x4a, extended: false },
  ShiftRight: { code: 0x59, extended: false },

  ControlLeft: { code: 0x14, extended: false },
  AltLeft: { code: 0x11, extended: false },
  Space: { code: 0x29, extended: false },

  ControlRight: { code: 0x14, extended: true },
  AltRight: { code: 0x11, extended: true },
  ArrowUp: { code: 0x75, extended: true },
  ArrowDown: { code: 0x72, extended: true },
  ArrowLeft: { code: 0x6b, extended: true },
  ArrowRight: { code: 0x74, extended: true },
  Home: { code: 0x6c, extended: true },
  End: { code: 0x69, extended: true },
  PageUp: { code: 0x7d, extended: true },
  PageDown: { code: 0x7a, extended: true },
  Insert: { code: 0x70, extended: true },
  Delete: { code: 0x71, extended: true },
  NumpadEnter: { code: 0x5a, extended: true },
  NumpadDivide: { code: 0x4a, extended: true },
};

export function codeToSet2(code: string): Set2Scancode | null {
  return SET2_BY_CODE[code] ?? null;
}

/**
 * Allocation-free mapping used by the batched input pipeline.
 *
 * Returns a Set 2 make code plus an "extended" flag:
 * - low 8 bits: make byte
 * - bit 8: extended (requires 0xE0 prefix)
 *
 * Returns 0 if the code is not mapped.
 */
export function translateCodeToSet2MakeCode(code: string): number {
  const sc = SET2_BY_CODE[code];
  if (!sc) return 0;
  return sc.code | (sc.extended ? 0x100 : 0);
}

export function set2Make(sc: Set2Scancode): number[] {
  return sc.extended ? [0xe0, sc.code] : [sc.code];
}

export function set2Break(sc: Set2Scancode): number[] {
  return sc.extended ? [0xe0, 0xf0, sc.code] : [0xf0, sc.code];
}

const DEFAULT_PREVENT_DEFAULT_CODES = new Set<string>([
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
]);

export function shouldPreventDefaultForKeyboardEvent(event: KeyboardEvent): boolean {
  if (event.ctrlKey || event.metaKey) {
    // Let browser shortcuts (copy/paste/tab close/etc.) win by default.
    return false;
  }
  return DEFAULT_PREVENT_DEFAULT_CODES.has(event.code);
}

