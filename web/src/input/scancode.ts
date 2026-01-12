import { PS2_SET2_CODE_TO_SCANCODE, type Ps2Set2Scancode } from "./scancodes";

export type Set2Scancode = {
  code: number;
  extended: boolean;
};

export function codeToSet2(code: string): Set2Scancode | null {
  const sc = PS2_SET2_CODE_TO_SCANCODE[code];
  if (!sc || sc.kind !== "simple") return null;
  return { code: sc.make, extended: sc.extended };
}

export function ps2Set2ScancodeForCode(code: string): Ps2Set2Scancode | undefined {
  return PS2_SET2_CODE_TO_SCANCODE[code];
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
  const sc = PS2_SET2_CODE_TO_SCANCODE[code];
  if (!sc || sc.kind !== "simple") return 0;
  return sc.make | (sc.extended ? 0x100 : 0);
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
  // Modifier keys that may otherwise trigger browser UI affordances (e.g. Alt menu focus).
  "AltLeft",
  "AltRight",
  // "Application"/context-menu key.
  "ContextMenu",
  // Function keys are commonly bound to browser actions (help, refresh, fullscreen).
  // Prevent them while the VM capture is active so they can be delivered to the guest.
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
]);

export function shouldPreventDefaultForKeyboardEvent(event: KeyboardEvent): boolean {
  if (event.ctrlKey || event.metaKey) {
    // Let browser shortcuts (copy/paste/tab close/etc.) win by default.
    return false;
  }
  return DEFAULT_PREVENT_DEFAULT_CODES.has(event.code);
}
