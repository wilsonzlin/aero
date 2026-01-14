/**
 * Snapshot pause helpers shared between the IO worker implementation and unit tests.
 *
 * The IO worker has asynchronous disk DMA operations that can mutate guest RAM after the main
 * device tick loop is paused. To take a consistent VM snapshot we must:
 *
 * 1) mark the worker as snapshot-paused (stops device ticking and blocks certain async callbacks)
 * 2) await any in-flight disk I/O operations (`diskIoChain`)
 * 3) only then ACK `vm.snapshot.paused`
 */
export async function pauseIoWorkerSnapshotAndDrainDiskIo(opts: {
  setSnapshotPaused: (paused: boolean) => void;
  setUsbProxyCompletionRingDispatchPaused: (paused: boolean) => void;
  diskIoChain: Promise<void>;
  onPaused: () => void;
}): Promise<void> {
  opts.setSnapshotPaused(true);
  opts.setUsbProxyCompletionRingDispatchPaused(true);
  await opts.diskIoChain.catch(() => undefined);
  opts.onPaused();
}

