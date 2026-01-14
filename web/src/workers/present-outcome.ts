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

export type PresentOutcomeDeltas = Readonly<{
  presentsSucceeded: 0 | 1;
  framesPresented: 0 | 1;
  framesDropped: 0 | 1;
}>;

const PRESENT_OUTCOME_PRESENTED: PresentOutcomeDeltas = {
  presentsSucceeded: 1,
  framesPresented: 1,
  framesDropped: 0,
};

const PRESENT_OUTCOME_DROPPED: PresentOutcomeDeltas = {
  presentsSucceeded: 0,
  framesPresented: 0,
  framesDropped: 1,
};

export function presentOutcomeDeltas(didPresent: boolean): PresentOutcomeDeltas {
  return didPresent ? PRESENT_OUTCOME_PRESENTED : PRESENT_OUTCOME_DROPPED;
}
