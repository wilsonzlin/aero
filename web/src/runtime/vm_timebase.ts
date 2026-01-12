/**
 * Snapshot-safe VM time base for worker device ticks.
 *
 * Many device models (notably audio DMA engines) advance guest-visible state based on host time
 * deltas (`nowMs` passed to `DeviceManager.tick`). During VM snapshot save/restore, the VM is
 * intentionally paused while the host wall clock continues to advance; if devices observe the
 * full wall-clock delta on resume they can "fast-forward" (burst audio output, DMA jumps, etc).
 *
 * This helper maintains a monotonic "VM tick time" that:
 * - tracks host time while the VM is running
 * - does *not* advance while `paused === true`
 * - supports resetting the host time base on resume (in case the device tick loop was stalled
 *   during pause and therefore did not observe intermediate host timestamps)
 */
export class VmTimebase {
  #hostLastNowMs: number | null = null;
  #vmNowMs: number | null = null;

  /**
   * Advance the VM time base and return the current VM-relative timestamp.
   *
   * When `paused` is true, the returned timestamp stays constant (but the host time base is still
   * updated so deltas after unpausing are small when ticks continue to run).
   */
  tick(hostNowMs: number, paused: boolean): number {
    if (this.#hostLastNowMs === null || this.#vmNowMs === null) {
      this.#hostLastNowMs = hostNowMs;
      this.#vmNowMs = hostNowMs;
      return hostNowMs;
    }

    let deltaHostMs = hostNowMs - this.#hostLastNowMs;
    this.#hostLastNowMs = hostNowMs;

    if (!Number.isFinite(deltaHostMs) || deltaHostMs <= 0) {
      deltaHostMs = 0;
    }

    if (!paused) {
      this.#vmNowMs += deltaHostMs;
    }

    return this.#vmNowMs;
  }

  /**
   * Reset the internal host time reference (typically on snapshot resume).
   *
   * If the tick loop does not run while paused (e.g. because the worker is blocked in a snapshot
   * streaming call), the next call to {@link tick} would otherwise see a large delta and advance
   * the VM time by wall-clock. Resetting `hostLastNowMs` ensures the next delta is ~0.
   */
  resetHostNowMs(hostNowMs: number): void {
    if (!Number.isFinite(hostNowMs)) return;
    this.#hostLastNowMs = hostNowMs;
    if (this.#vmNowMs === null) {
      this.#vmNowMs = hostNowMs;
    }
  }
}

