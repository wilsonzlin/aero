import { isValidHttpTokenPart } from "./httpTokens.js";

export function hasWebSocketSubprotocol(header, required) {
  if (!header) return { ok: true, has: false };

  // Defensive caps for websocket subprotocol parsing. This header is attacker-controlled and can be
  // joined/split in ways that allocate heavily if unconstrained.
  const MAX_SEC_WEBSOCKET_PROTOCOL_LEN = 4 * 1024;
  const MAX_SEC_WEBSOCKET_PROTOCOLS = 32;

  const parts = Array.isArray(header) ? header : [header];
  let totalLen = 0;
  for (const part of parts) {
    if (typeof part !== "string") return { ok: false, has: false };
    totalLen += part.length;
    if (totalLen > MAX_SEC_WEBSOCKET_PROTOCOL_LEN) return { ok: false, has: false };
  }

  let count = 0;
  let found = false;

  for (const part of parts) {
    let start = 0;
    while (start < part.length) {
      let end = part.indexOf(",", start);
      if (end === -1) end = part.length;

      // Trim ASCII whitespace.
      while (start < end && part.charCodeAt(start) <= 0x20) start += 1;
      while (end > start && part.charCodeAt(end - 1) <= 0x20) end -= 1;

      if (end > start) {
        count += 1;
        if (count > MAX_SEC_WEBSOCKET_PROTOCOLS) return { ok: false, has: false };
        if (!isValidHttpTokenPart(part, start, end)) return { ok: false, has: false };

        const len = end - start;
        if (len === required.length) {
          let ok = true;
          for (let i = 0; i < len; i += 1) {
            if (part.charCodeAt(start + i) !== required.charCodeAt(i)) {
              ok = false;
              break;
            }
          }
          if (ok) found = true;
        }
      }

      start = end + 1;
    }
  }

  return { ok: true, has: found };
}

