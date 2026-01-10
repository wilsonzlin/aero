import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";

export type CpuWorkerMockInitMessage = {
  type: "init";
  shared: SharedArrayBuffer;
  framebufferOffsetBytes: number;
  width: number;
  height: number;
  tileSize: number;
};

self.onmessage = (ev: MessageEvent<CpuWorkerMockInitMessage>) => {
  if (ev.data.type !== "init") return;

  const { shared, framebufferOffsetBytes, width, height, tileSize } = ev.data;
  const strideBytes = width * 4;

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

  setInterval(() => {
    const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
    const back = active ^ 1;

    const color = frameToggle ? 0xff0000ff /* RGBA red */ : 0xff00ff00 /* RGBA green */;
    frameToggle = !frameToggle;

    const backPixels = back === 0 ? slot0 : slot1;
    backPixels.fill(color);

    const backDirty = back === 0 ? dirty0 : dirty1;
    if (backDirty) {
      backDirty.fill(0xffffffff);
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

    // Wake the GPU worker (which waits on FRAME_SEQ).
    Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
  }, intervalMs);
};

