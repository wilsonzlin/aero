import { expect, test } from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

declare global {
  interface Window {
    __i8042Bytes: number[];
    __pressCode(code: string): void;
  }
}

type RawMapping = {
  ps2_set2: Record<
    string,
    {
      make: string[];
      break?: string[];
    }
  >;
};

function hexBytesToNumbers(bytes: string[]): number[] {
  return bytes.map((b) => Number.parseInt(b, 16));
}

test('KeyboardEvent.code → PS/2 set 2 bytes (i8042 feed)', async ({ page }) => {
  const testDir = path.dirname(fileURLToPath(import.meta.url));
  const repoRoot = path.resolve(testDir, '../..');

  const mappingPath = path.join(repoRoot, 'tools/gen_scancodes/scancodes.json');
  const raw = JSON.parse(await fs.readFile(mappingPath, 'utf8')) as RawMapping;

  const mapping = Object.fromEntries(
    Object.entries(raw.ps2_set2).map(([code, entry]) => [
      code,
      {
        make: hexBytesToNumbers(entry.make),
        break: entry.break ? hexBytesToNumbers(entry.break) : undefined,
      },
    ]),
  );

  await page.addInitScript(
    ({ mapping }) => {
      const w = window as unknown as Window;

      // Emulated i8042 "device" sink: collect all bytes that would be written to the controller.
      w.__i8042Bytes = [];

      type MappingEntry = { make: number[]; break?: number[] };
      const entries: Record<string, MappingEntry> = mapping;

      function bytesForKeyEvent(code: string, pressed: boolean): number[] | undefined {
        const entry = entries[code];
        if (!entry) return undefined;

        if (entry.break) return pressed ? entry.make.slice() : entry.break.slice();

        // Simple (1-byte) make, with optional 0xE0 prefix.
        if (entry.make.length === 1) {
          const make = entry.make[0];
          return pressed ? [make] : [0xf0, make];
        }
        if (entry.make.length === 2 && entry.make[0] === 0xe0) {
          const make = entry.make[1];
          return pressed ? [0xe0, make] : [0xe0, 0xf0, make];
        }

        throw new Error(`Non-simple key mapping for ${code} missing explicit break sequence`);
      }

      function onKeyEvent(ev: KeyboardEvent, pressed: boolean) {
        const bytes = bytesForKeyEvent(ev.code, pressed);
        if (bytes) w.__i8042Bytes.push(...bytes);
      }

      document.addEventListener('keydown', (ev) => onKeyEvent(ev, true));
      document.addEventListener('keyup', (ev) => onKeyEvent(ev, false));

      w.__pressCode = (code: string) => {
        document.dispatchEvent(new KeyboardEvent('keydown', { code, bubbles: true }));
        document.dispatchEvent(new KeyboardEvent('keyup', { code, bubbles: true }));
      };
    },
    { mapping },
  );

  await page.setContent('<!doctype html><meta charset="utf-8"><title>scancodes</title>');

  await page.evaluate(() => {
    window.__i8042Bytes.length = 0;

    // Unit-ish coverage, but executed in the browser event loop to validate our capture path.
    window.__pressCode('KeyA');
    window.__pressCode('Digit1');
    window.__pressCode('F12');

    window.__pressCode('ArrowUp');
    window.__pressCode('Insert');
    window.__pressCode('Delete');

    window.__pressCode('ControlRight');
    window.__pressCode('AltRight');
    window.__pressCode('NumpadEnter');
    window.__pressCode('NumpadDivide');
  });

  const bytes = await page.evaluate(() => window.__i8042Bytes);
  expect(bytes).toEqual([
    // KeyA
    0x1c, 0xf0, 0x1c,
    // Digit1
    0x16, 0xf0, 0x16,
    // F12
    0x07, 0xf0, 0x07,

    // ArrowUp (E0 75)
    0xe0, 0x75, 0xe0, 0xf0, 0x75,
    // Insert (E0 70)
    0xe0, 0x70, 0xe0, 0xf0, 0x70,
    // Delete (E0 71)
    0xe0, 0x71, 0xe0, 0xf0, 0x71,

    // Right Control (E0 14)
    0xe0, 0x14, 0xe0, 0xf0, 0x14,
    // Right Alt / AltGr (E0 11)
    0xe0, 0x11, 0xe0, 0xf0, 0x11,
    // NumpadEnter (E0 5A)
    0xe0, 0x5a, 0xe0, 0xf0, 0x5a,
    // NumpadDivide (E0 4A)
    0xe0, 0x4a, 0xe0, 0xf0, 0x4a,
  ]);
});

