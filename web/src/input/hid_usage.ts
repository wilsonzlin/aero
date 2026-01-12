/**
 * Map DOM `KeyboardEvent.code` strings (physical key positions) to USB HID usages
 * on the Keyboard/Keypad usage page (0x07).
 *
 * This is intentionally not exhaustive; it covers the keys needed for basic
 * desktop/login navigation and common alphanumerics.
 */
export function keyboardCodeToHidUsage(code: string): number | null {
  // Letters: KeyA..KeyZ => 0x04..0x1D
  if (code.startsWith("Key") && code.length === 4) {
    const c = code.charCodeAt(3);
    if (c >= 0x41 && c <= 0x5a) {
      return 0x04 + (c - 0x41);
    }
  }

  // Digits: Digit1..Digit9 => 0x1E..0x26, Digit0 => 0x27
  if (code.startsWith("Digit") && code.length === 6) {
    const c = code.charCodeAt(5);
    if (c >= 0x31 && c <= 0x39) {
      return 0x1e + (c - 0x31);
    }
    if (c === 0x30) {
      return 0x27;
    }
  }

  // Function keys: F1..F12 => 0x3A..0x45
  if (code.startsWith("F") && code.length >= 2 && code.length <= 3) {
    const n = Number.parseInt(code.slice(1), 10);
    if (Number.isFinite(n) && n >= 1 && n <= 12) {
      return 0x3a + (n - 1);
    }
  }

  switch (code) {
    // Modifiers (0xE0..=0xE7)
    case "ControlLeft":
      return 0xe0;
    case "ShiftLeft":
      return 0xe1;
    case "AltLeft":
      return 0xe2;
    case "MetaLeft":
    case "OSLeft":
      return 0xe3;
    case "ControlRight":
      return 0xe4;
    case "ShiftRight":
      return 0xe5;
    case "AltRight":
      return 0xe6;
    case "MetaRight":
    case "OSRight":
      return 0xe7;

    // Basic keys.
    case "Enter":
      return 0x28;
    case "Escape":
      return 0x29;
    case "Backspace":
      return 0x2a;
    case "Tab":
      return 0x2b;
    case "Space":
      return 0x2c;
    case "Minus":
      return 0x2d;
    case "Equal":
      return 0x2e;
    case "BracketLeft":
      return 0x2f;
    case "BracketRight":
      return 0x30;
    case "Backslash":
      return 0x31;
    case "IntlHash":
      return 0x32;
    case "Semicolon":
      return 0x33;
    case "Quote":
      return 0x34;
    case "Backquote":
      return 0x35;
    case "Comma":
      return 0x36;
    case "Period":
      return 0x37;
    case "Slash":
      return 0x38;
    case "CapsLock":
      return 0x39;

    // Navigation / system.
    case "PrintScreen":
      return 0x46;
    case "ScrollLock":
      return 0x47;
    case "Pause":
      return 0x48;
    case "Insert":
      return 0x49;
    case "Home":
      return 0x4a;
    case "PageUp":
      return 0x4b;
    case "Delete":
      return 0x4c;
    case "End":
      return 0x4d;
    case "PageDown":
      return 0x4e;
    case "ArrowRight":
      return 0x4f;
    case "ArrowLeft":
      return 0x50;
    case "ArrowDown":
      return 0x51;
    case "ArrowUp":
      return 0x52;

    // Keypad.
    case "NumLock":
      return 0x53;
    case "NumpadDivide":
      return 0x54;
    case "NumpadMultiply":
      return 0x55;
    case "NumpadSubtract":
      return 0x56;
    case "NumpadAdd":
      return 0x57;
    case "NumpadEnter":
      return 0x58;
    case "Numpad1":
      return 0x59;
    case "Numpad2":
      return 0x5a;
    case "Numpad3":
      return 0x5b;
    case "Numpad4":
      return 0x5c;
    case "Numpad5":
      return 0x5d;
    // Some browsers/keyboard layouts report the numpad 5 position as "NumpadClear".
    case "NumpadClear":
      return 0x5d;
    case "Numpad6":
      return 0x5e;
    case "Numpad7":
      return 0x5f;
    case "Numpad8":
      return 0x60;
    case "Numpad9":
      return 0x61;
    case "Numpad0":
      return 0x62;
    case "NumpadDecimal":
      return 0x63;
    case "NumpadEqual":
      return 0x67;
    case "NumpadComma":
      return 0x85;

    // "Application" key (aka context menu).
    case "ContextMenu":
      return 0x65;

    // International keys.
    case "IntlBackslash":
      return 0x64;
    case "IntlRo":
      return 0x87;
    case "IntlYen":
      return 0x89;
  }

  return null;
}
