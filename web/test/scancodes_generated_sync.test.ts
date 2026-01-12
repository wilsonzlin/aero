import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  PS2_SET2_CODE_TO_SCANCODE as WEB_PS2_SET2_CODE_TO_SCANCODE,
  ps2Set2BytesForKeyEvent as webPs2Set2BytesForKeyEvent,
} from '../src/input/scancodes.ts';
import {
  PS2_SET2_CODE_TO_SCANCODE as HARNESS_PS2_SET2_CODE_TO_SCANCODE,
  ps2Set2BytesForKeyEvent as harnessPs2Set2BytesForKeyEvent,
} from '../../src/input/scancodes.ts';

type JsonScancodeEntry = {
  make: string[];
  break?: string[];
};

type JsonScancodes = {
  ps2_set2: Record<string, JsonScancodeEntry>;
};

const REGEN_HINT =
  'If this is unexpected, regenerate the generated scancode tables by running `npm run gen:scancodes`.';

function parseHexByte(s: string): number {
  assert.match(
    s,
    /^[0-9A-Fa-f]{2}$/,
    `Invalid scancode byte ${JSON.stringify(s)} in tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
  );
  const n = Number.parseInt(s, 16);
  assert.ok(
    Number.isInteger(n) && n >= 0 && n <= 0xff,
    `Invalid scancode byte ${JSON.stringify(s)} in tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
  );
  return n;
}

function loadJsonScancodes(): JsonScancodes {
  const jsonPath = new URL('../../tools/gen_scancodes/scancodes.json', import.meta.url);
  const raw = readFileSync(jsonPath, 'utf8');
  const parsed = JSON.parse(raw) as unknown;
  assert.ok(
    parsed && typeof parsed === 'object' && 'ps2_set2' in parsed,
    `Expected tools/gen_scancodes/scancodes.json to contain a top-level "ps2_set2" object. ${REGEN_HINT}`,
  );
  return parsed as JsonScancodes;
}

function assertTsMappingInSync(
  label: string,
  tsMapping: Record<string, unknown>,
  tsBytesForKeyEvent: (code: string, pressed: boolean) => number[] | undefined,
  jsonScancodes: JsonScancodes,
): void {
  const jsonMapping = jsonScancodes.ps2_set2;
  const jsonCodes = Object.keys(jsonMapping).sort();
  const tsCodes = Object.keys(tsMapping).sort();

  assert.deepStrictEqual(
    tsCodes,
    jsonCodes,
    `${label}: exported PS2_SET2_CODE_TO_SCANCODE keys do not match tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
  );

  for (const [code, entry] of Object.entries(jsonMapping)) {
    const actual = tsMapping[code] as
      | { kind: 'simple'; make: number; extended: boolean }
      | { kind: 'sequence'; make: readonly number[]; break: readonly number[] }
      | undefined;

    if (!actual) {
      assert.fail(
        `${label}: missing PS/2 Set-2 scancode mapping for ${JSON.stringify(code)}. ${REGEN_HINT}`,
      );
    }

    const makeBytes = entry.make.map(parseHexByte);
    const breakBytes = entry.break?.map(parseHexByte);

    const isExtendedSimple = makeBytes.length === 2 && makeBytes[0] === 0xe0;
    const isNonExtendedSimple = makeBytes.length === 1;

    if (isNonExtendedSimple || isExtendedSimple) {
      const expected = {
        kind: 'simple' as const,
        make: makeBytes[isExtendedSimple ? 1 : 0],
        extended: isExtendedSimple,
      };

      assert.deepStrictEqual(
        actual,
        expected,
        `${label}: PS2_SET2_CODE_TO_SCANCODE[${JSON.stringify(code)}] is out of sync with tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
      );
    } else {
      assert.ok(
        breakBytes !== undefined,
        `${label}: tools/gen_scancodes/scancodes.json entry for ${JSON.stringify(code)} has a multi-byte make sequence but no explicit break sequence. ${REGEN_HINT}`,
      );

      if (actual.kind !== 'sequence') {
        assert.fail(
          `${label}: PS2_SET2_CODE_TO_SCANCODE[${JSON.stringify(code)}] should be a { kind: 'sequence', ... } scancode. ${REGEN_HINT}`,
        );
      }

      assert.deepStrictEqual(
        actual.make,
        makeBytes,
        `${label}: PS2_SET2_CODE_TO_SCANCODE[${JSON.stringify(code)}].make is out of sync with tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
      );
      assert.deepStrictEqual(
        actual.break,
        breakBytes,
        `${label}: PS2_SET2_CODE_TO_SCANCODE[${JSON.stringify(code)}].break is out of sync with tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
      );
    }

    const expectedBreakBytes =
      breakBytes ??
      (makeBytes.length === 1
        ? [0xf0, makeBytes[0]]
        : makeBytes.length === 2 && makeBytes[0] === 0xe0
          ? [0xe0, 0xf0, makeBytes[1]]
          : undefined);

    if (!expectedBreakBytes) {
      assert.fail(
        `${label}: could not derive default break bytes for ${JSON.stringify(code)} from tools/gen_scancodes/scancodes.json make=${JSON.stringify(entry.make)}. ${REGEN_HINT}`,
      );
    }

    assert.deepStrictEqual(
      tsBytesForKeyEvent(code, true),
      makeBytes,
      `${label}: ps2Set2BytesForKeyEvent(${JSON.stringify(code)}, true) is out of sync with tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
    );
    assert.deepStrictEqual(
      tsBytesForKeyEvent(code, false),
      expectedBreakBytes,
      `${label}: ps2Set2BytesForKeyEvent(${JSON.stringify(code)}, false) is out of sync with tools/gen_scancodes/scancodes.json. ${REGEN_HINT}`,
    );
  }
}

test('generated PS/2 Set-2 scancode tables are in sync with tools/gen_scancodes/scancodes.json', () => {
  const jsonScancodes = loadJsonScancodes();

  assertTsMappingInSync(
    'web/src/input/scancodes.ts',
    WEB_PS2_SET2_CODE_TO_SCANCODE,
    (code, pressed) => webPs2Set2BytesForKeyEvent(code, pressed),
    jsonScancodes,
  );
  assertTsMappingInSync(
    'src/input/scancodes.ts',
    HARNESS_PS2_SET2_CODE_TO_SCANCODE,
    (code, pressed) => harnessPs2Set2BytesForKeyEvent(code, pressed),
    jsonScancodes,
  );
});

test("scancode tables cover standard, extended (E0), and multi-byte sequences", () => {
  // Standard keys (letters/digits).
  for (const [code, make] of [
    ["KeyA", [0x1c]],
    ["Digit1", [0x16]],
  ] as const) {
    assert.deepStrictEqual(webPs2Set2BytesForKeyEvent(code, true), make);
    assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent(code, true), make);

    // For non-extended, non-sequence keys, break is the canonical Set-2 `F0 <make>` form.
    assert.deepStrictEqual(webPs2Set2BytesForKeyEvent(code, false), [0xf0, make[0]]);
    assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent(code, false), [0xf0, make[0]]);
  }

  // Extended keys (E0 prefix).
  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("ArrowUp", true), [0xe0, 0x75]);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("ArrowUp", true), [0xe0, 0x75]);
  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("ArrowUp", false), [0xe0, 0xf0, 0x75]);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("ArrowUp", false), [0xe0, 0xf0, 0x75]);

  // Multi-byte sequences (PrintScreen / Pause).
  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("PrintScreen", true), [0xe0, 0x12, 0xe0, 0x7c]);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("PrintScreen", true), [0xe0, 0x12, 0xe0, 0x7c]);
  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("PrintScreen", false), [0xe0, 0xf0, 0x7c, 0xe0, 0xf0, 0x12]);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("PrintScreen", false), [0xe0, 0xf0, 0x7c, 0xe0, 0xf0, 0x12]);

  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("Pause", true), [
    0xe1, 0x14, 0x77, 0xe1, 0xf0, 0x14, 0xf0, 0x77,
  ]);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("Pause", true), [
    0xe1, 0x14, 0x77, 0xe1, 0xf0, 0x14, 0xf0, 0x77,
  ]);
  assert.deepStrictEqual(webPs2Set2BytesForKeyEvent("Pause", false), []);
  assert.deepStrictEqual(harnessPs2Set2BytesForKeyEvent("Pause", false), []);
});
