import test from "node:test";
import assert from "node:assert/strict";

import { wsCloseSafe } from "../wsClose";

function hasForbiddenControlChars(s: string): boolean {
  for (const ch of s) {
    const code = ch.codePointAt(0) ?? 0;
    if (code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029) {
      return true;
    }
  }
  return false;
}

test("wsCloseSafe sanitizes close reasons to avoid control characters", () => {
  let gotCode: number | null = null;
  let gotReason: string | null = null;

  const ws = {
    close(code: number, reason?: string) {
      gotCode = code;
      gotReason = reason ?? "";
    }
  } as unknown as import("ws").WebSocket;

  wsCloseSafe(ws, 1008, "bad\r\nreason\twith\u2028separators\u0000and\x7fcontrols");

  assert.equal(gotCode, 1008);
  const reason = gotReason ?? "";
  assert.equal(hasForbiddenControlChars(reason), false);
  assert.equal(reason.includes("\n"), false);
  assert.equal(reason.includes("\r"), false);
  assert.match(reason, /^bad reason with separators and controls$/);
});

