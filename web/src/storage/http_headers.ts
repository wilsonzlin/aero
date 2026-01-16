function asciiLowerCode(code: number): number {
  return code >= 0x41 && code <= 0x5a ? code + 0x20 : code;
}

function asciiLowerEqualsSpan(value: string, start: number, end: number, expectedLower: string): boolean {
  if (end - start !== expectedLower.length) return false;
  for (let i = 0; i < expectedLower.length; i += 1) {
    if (asciiLowerCode(value.charCodeAt(start + i)) !== expectedLower.charCodeAt(i)) return false;
  }
  return true;
}

function isTcharCode(code: number): boolean {
  // RFC 7230 tchar:
  // "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
  if (code >= 0x30 && code <= 0x39) return true; // 0-9
  if (code >= 0x41 && code <= 0x5a) return true; // A-Z
  if (code >= 0x61 && code <= 0x7a) return true; // a-z
  switch (code) {
    case 0x21: // !
    case 0x23: // #
    case 0x24: // $
    case 0x25: // %
    case 0x26: // &
    case 0x27: // '
    case 0x2a: // *
    case 0x2b: // +
    case 0x2d: // -
    case 0x2e: // .
    case 0x5e: // ^
    case 0x5f: // _
    case 0x60: // `
    case 0x7c: // |
    case 0x7e: // ~
      return true;
    default:
      return false;
  }
}

export const MAX_CONTENT_ENCODING_HEADER_VALUE_LEN = 256;
export const MAX_CACHE_CONTROL_HEADER_VALUE_LEN = 4 * 1024;
export const MAX_CONTENT_RANGE_HEADER_VALUE_LEN = 256;

export function formatHeaderValueForError(value: string, maxPreviewLen = 128): string {
  if (maxPreviewLen <= 0) return `(${value.length} chars)`;
  if (value.length <= maxPreviewLen) return value;
  return `${value.slice(0, maxPreviewLen)}…(${value.length} chars)`;
}

export type TokenListParseOptions = {
  maxLen: number;
};

export function commaSeparatedTokenListHasToken(
  value: string,
  tokenLower: string,
  opts: TokenListParseOptions,
): boolean {
  if (value.length > opts.maxLen) return false;

  const len = value.length;
  let i = 0;
  while (i < len) {
    // Skip separators / OWS
    while (i < len) {
      const c = value.charCodeAt(i);
      if (c === 0x20 /* space */ || c === 0x09 /* tab */ || c === 0x2c /* , */) i += 1;
      else break;
    }

    const start = i;
    while (i < len && isTcharCode(value.charCodeAt(i))) i += 1;
    const end = i;

    if (end > start && asciiLowerEqualsSpan(value, start, end, tokenLower)) return true;

    // Skip to the next comma (or end); Cache-Control directives can include parameters.
    while (i < len && value.charCodeAt(i) !== 0x2c /* , */) i += 1;
    if (i < len && value.charCodeAt(i) === 0x2c /* , */) i += 1;
  }

  return false;
}

export type ContentEncodingOptions = {
  maxLen: number;
};

export function contentEncodingIsIdentity(value: string, opts: ContentEncodingOptions): boolean {
  if (value.length > opts.maxLen) return false;
  let start = 0;
  let end = value.length;
  while (start < end) {
    const c = value.charCodeAt(start);
    if (c === 0x20 /* space */ || c === 0x09 /* tab */) start += 1;
    else break;
  }
  while (end > start) {
    const c = value.charCodeAt(end - 1);
    if (c === 0x20 /* space */ || c === 0x09 /* tab */) end -= 1;
    else break;
  }
  if (end <= start) return true;
  return asciiLowerEqualsSpan(value, start, end, "identity");
}

