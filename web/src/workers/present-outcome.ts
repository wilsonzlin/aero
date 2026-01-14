/**
 * Interpret the return value contract for presenter backends.
 *
 * Presenters may return `false` to indicate that the frame was intentionally
 * dropped (not presented), e.g. due to a surface acquire timeout or a recoverable
 * surface error.
 *
 * `undefined` (historical behavior) and `true` are treated as success.
 */
export function didPresenterPresent(result: void | boolean): boolean {
  return result !== false;
}

