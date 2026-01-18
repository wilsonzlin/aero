import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import { FRAME_DIRTY, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "../ipc/gpu-protocol";
import { unrefBestEffort } from "../unrefSafe";

export type CpuWorkerMockInitMessage = {
  type: "init";
  shared: SharedArrayBuffer;
  framebufferOffsetBytes: number;
  /**
   * Optional SharedArrayBuffer used for main-thread ↔ GPU-worker pacing state.
   *
   * When provided, the mock will bump the shared sequence and mark the frame as
   * DIRTY when publishing a new framebuffer slot.
   */
  sharedFrameState?: SharedArrayBuffer;
  width: number;
  height: number;
  tileSize: number;
  /**
   * Controls how the mock generates frames.
   *
   * - "solid": alternate solid red/green frames (default; exercises double-buffer publish).
   * - "tile_toggle": keep the frame mostly green but toggle the top-left tile; dirty tiles
   *   after the first frame will only mark that tile (exercises partial uploads).
   */
  pattern?: "solid" | "tile_toggle";
};

self.onmessage = (ev: MessageEvent<CpuWorkerMockInitMessage>) => {
  if (ev.data.type !== "init") return;

  const { shared, framebufferOffsetBytes, width, height, tileSize } = ev.data;
  const pattern = ev.data.pattern ?? "solid";
  const strideBytes = width * 4;
  const frameState = ev.data.sharedFrameState ? new Int32Array(ev.data.sharedFrameState) : null;

  const layout = computeSharedFramebufferLayout(width, height, strideBytes, FramebufferFormat.RGBA8, tileSize);

  const header = new Int32Array(shared, framebufferOffsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

  // Initialize the atomic header.
  Atomics.store(header, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(header, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, width);
  Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, height);
  Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, strideBytes);
  Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, FramebufferFormat.RGBA8);
  Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, tileSize);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_X, layout.tilesX);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_Y, layout.tilesY);
  Atomics.store(header, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, layout.dirtyWordsPerBuffer);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FLAGS, 0);

  const slot0 = new Uint32Array(shared, framebufferOffsetBytes + layout.framebufferOffsets[0], (strideBytes / 4) * height);
  const slot1 = new Uint32Array(shared, framebufferOffsetBytes + layout.framebufferOffsets[1], (strideBytes / 4) * height);

  const dirty0 =
    layout.dirtyWordsPerBuffer === 0
      ? null
      : new Uint32Array(shared, framebufferOffsetBytes + layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
  const dirty1 =
    layout.dirtyWordsPerBuffer === 0
      ? null
      : new Uint32Array(shared, framebufferOffsetBytes + layout.dirtyOffsets[1], layout.dirtyWordsPerBuffer);

  let frameToggle = false;
  const intervalMs = Math.floor(1000 / 30);

  const timer = setInterval(() => {
    // `frame_dirty` is a producer->consumer "new frame" / liveness flag. Consumers may clear it
    // after they finish copying/presenting; treat it as a best-effort ACK and throttle publishing
    // so we don't overwrite a buffer that might still be read by the presenter.
    if (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_DIRTY) !== 0) {
      return;
    }

    const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
    const back = active ^ 1;

    const backPixels = back === 0 ? slot0 : slot1;
    const backDirty = back === 0 ? dirty0 : dirty1;

    if (pattern === "tile_toggle") {
      const base = 0xff00ff00; // green
      const tile = frameToggle ? 0xff0000ff /* red */ : base;
      frameToggle = !frameToggle;

      // Full frame render with a small changing region (top-left tile).
      backPixels.fill(base);
      const tileW = Math.min(tileSize || width, width);
      const tileH = Math.min(tileSize || height, height);
      for (let y = 0; y < tileH; y += 1) {
        const row = y * width;
        backPixels.fill(tile, row, row + tileW);
      }

      if (backDirty) {
        // Frame 1 initializes the texture, so treat as full-frame dirty.
        const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
        if (seq === 0) {
          backDirty.fill(0xffffffff);
        } else {
          backDirty.fill(0);
          // Mark only tile 0 dirty (top-left).
          backDirty[0] = 1;
        }
      }
    } else {
      const color = frameToggle ? 0xff0000ff /* RGBA red */ : 0xff00ff00 /* RGBA green */;
      frameToggle = !frameToggle;

      backPixels.fill(color);
      if (backDirty) {
        backDirty.fill(0xffffffff);
      }
    }

    const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;

    Atomics.store(
      header,
      back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
      newSeq,
    );
    Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);

    if (frameState) {
      Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
      Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
    }

    // Wake the GPU worker (which waits on FRAME_SEQ).
    Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
  }, intervalMs);
  unrefBestEffort(timer);
};
