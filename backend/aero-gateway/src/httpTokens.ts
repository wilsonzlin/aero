export function isTchar(code: number): boolean {
  // RFC 7230 tchar
  // "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
  // "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
  if (code >= 0x30 && code <= 0x39) return true; // 0-9
  if (code >= 0x41 && code <= 0x5a) return true; // A-Z
  if (code >= 0x61 && code <= 0x7a) return true; // a-z
  return (
    code === 0x21 || // !
    code === 0x23 || // #
    code === 0x24 || // $
    code === 0x25 || // %
    code === 0x26 || // &
    code === 0x27 || // '
    code === 0x2a || // *
    code === 0x2b || // +
    code === 0x2d || // -
    code === 0x2e || // .
    code === 0x5e || // ^
    code === 0x5f || // _
    code === 0x60 || // `
    code === 0x7c || // |
    code === 0x7e // ~
  );
}

export function isValidHttpTokenPart(input: string, start: number, end: number): boolean {
  // token = 1*tchar
  if (end <= start) return false;
  for (let i = start; i < end; i += 1) {
    if (!isTchar(input.charCodeAt(i))) return false;
  }
  return true;
}

export function isValidHttpToken(token: string): boolean {
  return isValidHttpTokenPart(token, 0, token.length);
}

