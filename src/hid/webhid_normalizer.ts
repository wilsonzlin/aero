export interface WebHidReportItemLike {
  /**
   * WebHID `HIDReportItem.isRange`.
   *
   * When true, `usages` is expected to be the expanded list of usages covered by
   * the range (e.g. keyboard modifiers `0xE0..=0xE7` -> `[0xE0, ..., 0xE7]`).
   *
   * Some sources may still provide the legacy `[min, max]` representation; we
   * treat that as a range as well.
   */
  isRange: boolean;
  usages: number[];
}

export interface NormalizeWebHidReportItemOptions {
  /**
   * When `isRange` is true and `usages.length` is <= this limit, check that the
   * list is contiguous. If it is not, the normalizer will downgrade `isRange`
   * to false.
   */
  maxContiguityCheckLen?: number;

  onWarn?: (message: string) => void;
}

const DEFAULT_MAX_CONTIGUITY_CHECK_LEN = 4096;

function isContiguousUsageRange(usages: number[]): boolean {
  if (usages.length === 0) return true;

  const sorted = Array.from(new Set(usages)).sort((a, b) => a - b);
  const min = sorted[0]!;
  const max = sorted[sorted.length - 1]!;
  if (min === max) return true;

  // Support both WebHID expanded ranges and legacy `[min, max]`.
  if (sorted.length === 2) return true;

  const span = max - min + 1;
  if (span !== sorted.length) return false;
  for (let i = 0; i < sorted.length; i++) {
    if (sorted[i] !== min + i) return false;
  }
  return true;
}

export function normalizeWebHidReportItem<T extends WebHidReportItemLike>(
  item: T,
  opts: NormalizeWebHidReportItemOptions = {},
): T {
  const maxLen = opts.maxContiguityCheckLen ?? DEFAULT_MAX_CONTIGUITY_CHECK_LEN;
  const usages = Array.isArray(item.usages) ? item.usages.slice() : [];
  const isRange = !!item.isRange;

  if (!isRange) {
    return { ...item, isRange, usages } as T;
  }

  // WebHID can legally return `usages.length === 1` for degenerate ranges.
  if (usages.length <= 1) {
    return { ...item, isRange, usages } as T;
  }

  if (usages.length <= maxLen && !isContiguousUsageRange(usages)) {
    opts.onWarn?.(
      `HIDReportItem.isRange is true but usages is not contiguous (len=${usages.length}); downgrading to isRange=false`,
    );
    return { ...item, isRange: false, usages } as T;
  }

  return { ...item, isRange, usages } as T;
}

