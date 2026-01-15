export interface SplitCommaSeparatedListOptions {
  maxLen: number;
  maxItems: number;
}

const DEFAULT_OPTS: SplitCommaSeparatedListOptions = {
  maxLen: 256 * 1024,
  maxItems: 4096
};

/**
 * Splits a comma-separated list, trimming whitespace around each entry and skipping empty entries.
 *
 * This is intended for parsing trusted configuration strings defensively (e.g. env vars) while
 * bounding work for accidental huge values.
 */
export function splitCommaSeparatedList(
  raw: string,
  opts: Partial<SplitCommaSeparatedListOptions> = {}
): string[] {
  const maxLen = opts.maxLen ?? DEFAULT_OPTS.maxLen;
  const maxItems = opts.maxItems ?? DEFAULT_OPTS.maxItems;

  if (!Number.isFinite(maxLen) || maxLen < 0) throw new Error("Invalid maxLen");
  if (!Number.isFinite(maxItems) || maxItems < 0) throw new Error("Invalid maxItems");

  if (raw.length > maxLen) throw new Error("Value too long");

  const out: string[] = [];
  let i = 0;
  while (i < raw.length) {
    let start = i;
    while (i < raw.length && raw.charCodeAt(i) !== 0x2c) i += 1; // ','
    let end = i;
    if (i < raw.length && raw.charCodeAt(i) === 0x2c) i += 1; // skip ','

    const token = raw.slice(start, end).trim();
    if (token.length > 0) out.push(token);
    if (out.length > maxItems) throw new Error("Too many entries");
  }

  return out;
}
