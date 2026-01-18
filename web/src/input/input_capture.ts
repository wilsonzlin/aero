import { InputEventQueue, type InputBatchFlushHook, type InputBatchRecycleMessage, type InputBatchTarget } from "./event_queue";
import { GamepadCapture } from "./gamepad";
import { PointerLock } from "./pointer_lock";
import { keyboardCodeToConsumerUsage, keyboardCodeToHidUsage } from "./hid_usage";
import { I32_MAX, I32_MIN, negateI32Saturating } from "./int32";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";
import {
  ps2Set2ScancodeForCode,
  shouldPreventDefaultForKeyboardEvent,
} from "./scancode";
import type { Ps2Set2Scancode } from "./scancodes";

export interface PointerLockReleaseChord {
  code: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
}

export interface InputCaptureOptions {
  /**
   * Multiply mouse movement by this scaling factor.
   *
   * Note: We keep fractional remainder to avoid losing sub-unit motion.
   */
  mouseSensitivity?: number;
  /**
   * Multiply touch drag deltas by this scaling factor before converting to
   * relative mouse motion.
   *
   * Defaults to `mouseSensitivity`.
   */
  touchSensitivity?: number;
  /**
   * How often to flush input to the target worker.
   *
   * 125Hz matches the classic PS/2 mouse sample rate and keeps latency <8ms
   * typical.
   */
  flushHz?: number;
  /**
   * Optional host-only chord that will exit pointer lock.
   *
   * If set, the chord is swallowed (not forwarded to the guest).
   */
  releasePointerLockChord?: PointerLockReleaseChord;
  /**
   * If enabled, logs host-side latency from the first event timestamp in a batch
   * to the moment the batch is `postMessage`d to the worker.
   *
   * This is not the full "event → guest IRQ" latency, but helps ensure we stay
   * under one frame (16ms) in typical cases.
   */
  logCaptureLatency?: boolean;
  /**
   * If enabled, request that the target worker transfers input batch buffers back for reuse. This
   * avoids allocating a new ArrayBuffer per flush.
   *
   * The worker must support `{ type: "in:input-batch", recycle: true }` and
   * respond with `{ type: "in:input-batch-recycle", buffer }`.
   */
  recycleBuffers?: boolean;
  /**
   * If enabled, polls the Gamepad API and emits USB HID gamepad reports.
   */
  enableGamepad?: boolean;
  /**
   * If enabled and supported by the browser, attempts to use the Keyboard Lock API
   * (`navigator.keyboard.lock()`) while capture is active.
   *
   * This is best-effort: failures (e.g. API unavailable, permission denied, or
   * user-gesture requirements not met) are ignored after logging.
   *
   * Note: locking `Escape` can prevent the browser's default "Escape exits pointer lock"
   * behavior. Apps should ensure there is an alternative way to leave capture (e.g. a
   * host-only `releasePointerLockChord` or an on-screen exit control).
   */
  enableKeyboardLock?: boolean;
  /**
   * Optional hook invoked immediately before each input batch is posted to the
   * worker.
   *
   * This is intended for debug tooling (e.g. deterministic record/replay) and
   * must not allocate on the hot path when disabled.
   */
  onBeforeSendBatch?: InputBatchFlushHook;
  /**
   * Analog stick deadzone in normalized units ([0, 1]).
   */
  gamepadDeadzone?: number;
  /**
   * How often to poll the Gamepad API (max: `flushHz`).
   */
  gamepadPollHz?: number;
  /**
   * If enabled, installs a best-effort touch → mouse fallback for environments
   * where Pointer Lock is unavailable or impractical (mobile, embedded browsers,
   * restrictive policies, etc).
   *
   * The fallback only applies while Pointer Lock is not active.
   */
  enableTouchFallback?: boolean;
  /**
   * If enabled, treat a quick, stationary tap as a left click (MouseButtons down+up).
   *
   * The tap gesture is recognized when the primary touch ends within a short
   * time window and without moving beyond a small threshold.
   */
  touchTapToClick?: boolean;
}

const MAX_RECYCLED_BUFFER_BYTES = 4 * 1024 * 1024;
const MAX_RECYCLED_BUCKET_SIZES = 8;
const MAX_RECYCLED_BUFFERS_PER_BUCKET = 4;

export class InputCapture {
  private readonly queue: InputEventQueue;
  private readonly pointerLock: PointerLock;
  private readonly pressedCodes = new Set<string>();
  // Host-only keys that were intentionally swallowed (e.g. pointer lock release chord) and whose
  // corresponding keyup should also be swallowed to avoid delivering a stray "break" event to the
  // guest.
  private readonly suppressedKeyUps = new Set<string>();
  private readonly gamepad: GamepadCapture | null;
  private readonly gamepadPollIntervalMs: number;
  private gamepadLastPollMs = 0;

  private readonly flushHz: number;
  private readonly mouseSensitivity: number;
  private readonly touchSensitivity: number;
  private readonly releaseChord?: PointerLockReleaseChord;
  private readonly logCaptureLatency: boolean;
  private readonly recycleBuffers: boolean;
  private readonly enableTouchFallback: boolean;
  private readonly touchTapToClick: boolean;
  private readonly onBeforeSendBatch?: InputBatchFlushHook;
  private readonly flushOpts: { recycle?: boolean; onBeforeSend?: InputBatchFlushHook };
  private readonly flushOptsNoRecycle: { recycle?: boolean; onBeforeSend?: InputBatchFlushHook };
  private readonly enableKeyboardLock: boolean;
  private keyboardLockRequested = false;
  private keyboardLockActive = false;
  private keyboardLockSeq = 0;

  private readonly recycledBuffersBySize = new Map<number, ArrayBuffer[]>();
  private readonly handleWorkerMessage = (event: MessageEvent<unknown>): void => {
    if (!this.recycleBuffers) {
      return;
    }
    const data = event.data as Partial<InputBatchRecycleMessage> | undefined;
    if (!data || data.type !== "in:input-batch-recycle") {
      return;
    }
    if (!(data.buffer instanceof ArrayBuffer)) {
      return;
    }
    const size = data.buffer.byteLength;
    if (size === 0 || size > MAX_RECYCLED_BUFFER_BYTES) {
      return;
    }

    const bucket = this.recycledBuffersBySize.get(size);
    if (bucket) {
      if (bucket.length >= MAX_RECYCLED_BUFFERS_PER_BUCKET) {
        // Bucket is full; drop the newly returned buffer (instead of evicting an existing one).
        return;
      }
      bucket.push(data.buffer);
      return;
    }

    if (this.recycledBuffersBySize.size >= MAX_RECYCLED_BUCKET_SIZES) {
      // Drop the oldest bucket (and all its buffers) to keep memory bounded.
      const oldest = this.recycledBuffersBySize.keys().next();
      if (!oldest.done) {
        this.recycledBuffersBySize.delete(oldest.value);
      }
    }

    if (this.recycledBuffersBySize.size >= MAX_RECYCLED_BUCKET_SIZES) {
      // Still full (e.g. limit is 0); drop the buffer.
      return;
    }
    this.recycledBuffersBySize.set(size, [data.buffer]);
  };

  private flushTimer: number | null = null;

  private hasFocus = false;
  private windowFocused = typeof document !== "undefined" ? document.hasFocus() : true;
  private pageVisible = typeof document !== "undefined" ? document.visibilityState !== "hidden" : true;

  private mouseButtons = 0;
  private mouseFracX = 0;
  private mouseFracY = 0;
  private wheelFrac = 0;
  private wheelFracX = 0;

  private touchActiveId: number | null = null;
  private touchLastX = 0;
  private touchLastY = 0;

  private touchPrimaryPointerId: number | null = null;
  private touchStartX = 0;
  private touchStartY = 0;
  private touchStartTimeStamp = 0;
  private touchMaxDistSq = 0;
  private touchHadMultiTouch = false;
  private readonly touchPointers = new Map<number, { x: number; y: number }>();
  private prevCanvasTouchAction: string | null = null;

  private latencyLogLastMs = 0;
  private latencyLogCount = 0;
  private latencyLogSumUs = 0;
  private latencyLogMaxUs = 0;

  private readonly handlePointerLockError = (): void => {
    // Pointer lock can fail (or be denied) even after a user gesture. If we attempted to acquire
    // keyboard lock for this capture session, release it so the host isn't left with unexpectedly
    // swallowed system keys.
    this.releaseKeyboardLock();
  };

  private readonly handlePointerLockChange = (locked: boolean): void => {
    // Pointer lock boundaries are also keyboard-lock boundaries: if the browser exits pointer lock
    // (e.g. Escape), drop keyboard lock so we don't keep swallowing system-reserved keys.
    if (!locked) {
      this.releaseKeyboardLock();
    } else {
      this.updateKeyboardLock();
    }

    // If pointer lock exits while the canvas is not focused, we will stop
    // capturing keyboard/mouse events, which can leave the guest with latched
    // input state. Emit a best-effort "all released" snapshot immediately.
    if (!locked) {
      // Pointer lock boundaries are a natural "capture session" boundary: drop any fractional
      // deltas from the prior session so they can't leak into a later session and cause a
      // spurious pixel or wheel tick.
      this.resetAccumulatedMotion();
      this.cancelTouchCapture();
      this.clearTouchState();
    }

    if (locked || this.hasFocus) {
      return;
    }

    const nowUs = toTimestampUs(performance.now());
    this.suppressedKeyUps.clear();
    this.clearTouchState();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, nowUs);
    try {
      this.queue.flush(this.ioWorker, this.flushOpts);
    } catch {
      // Best-effort only; the worker may have been terminated during teardown.
    }
  };

  private readonly handleClick = (event: MouseEvent): void => {
    // Swallow clicks on the canvas while capture is active. `mousedown`/`mouseup` handlers already
    // preventDefault+stopPropagation, but the synthesized `click` event is distinct and would
    // otherwise bubble to app-level handlers (e.g. toggles or global click listeners).
    event.preventDefault();
    event.stopPropagation();
    // Best-effort: request Keyboard Lock for this capture session. The API requires a user gesture,
    // so mark the request before any focus/pointer-lock side effects and attempt the lock while
    // we're still in the click handler.
    if (this.pointerLock.isSupported) {
      this.keyboardLockRequested = true;
    }
    this.canvas.focus();
    this.pointerLock.request();
    this.updateKeyboardLock();
  };

  private readonly handleFocus = (): void => {
    this.hasFocus = true;
    this.updateKeyboardLock();
  };

  private readonly handleBlur = (): void => {
    this.hasFocus = false;
    this.releaseKeyboardLock();
    this.pointerLock.exit();
    this.clearTouchState();
    this.resetAccumulatedMotion();
    this.cancelTouchCapture();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    // Flush immediately; timers may be throttled in the background and we don't
    // want the guest to observe "stuck" inputs.
    try {
      this.queue.flush(this.ioWorker, this.flushOpts);
    } catch {
      // Best-effort only; the worker may have been terminated during teardown.
    }
  };

  private readonly handleWindowBlur = (): void => {
    this.windowFocused = false;
    this.releaseKeyboardLock();
    this.pointerLock.exit();
    this.clearTouchState();
    this.resetAccumulatedMotion();
    this.cancelTouchCapture();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    try {
      this.queue.flush(this.ioWorker, this.flushOpts);
    } catch {
      // Best-effort only; the worker may have been terminated during teardown.
    }
  };

  private readonly handleWindowFocus = (): void => {
    this.windowFocused = true;
  };

  private readonly handleVisibilityChange = (): void => {
    this.pageVisible = document.visibilityState !== "hidden";
    if (this.pageVisible) {
      return;
    }

    this.releaseKeyboardLock();
    this.pointerLock.exit();
    this.clearTouchState();
    this.resetAccumulatedMotion();
    this.cancelTouchCapture();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    try {
      this.queue.flush(this.ioWorker, this.flushOpts);
    } catch {
      // Best-effort only; the worker may have been terminated during teardown.
    }
  };

  private readonly handleKeyDown = (event: KeyboardEvent): void => {
    if (!this.isCapturingKeyboard()) {
      return;
    }

    if (!event.repeat) {
      // If a prior host-only chord swallowed a keydown but the browser never delivered the
      // corresponding keyup (common for Escape used to exit pointer lock), we may have a stale
      // suppression entry. Clear it on the next non-repeat keydown so subsequent keyup events are
      // not accidentally swallowed (which would otherwise produce a "stuck key" in the guest).
      this.suppressedKeyUps.delete(event.code);
    }

    if (this.suppressedKeyUps.has(event.code)) {
      // A host-only chord already swallowed a keydown for this key and we're still waiting for the
      // corresponding keyup. Swallow any repeat keydown events so the guest never sees a make
      // without a break (which would otherwise produce a stuck key).
      event.preventDefault();
      event.stopPropagation();
      return;
    }

    if (this.pointerLock.isLocked && this.releaseChord && chordMatches(event, this.releaseChord)) {
      event.preventDefault();
      event.stopPropagation();
      this.suppressedKeyUps.add(event.code);
      this.pointerLock.exit();
      return;
    }

    const shouldPreventDefault = shouldPreventDefaultForKeyboardEvent(event);

    const sc = ps2Set2ScancodeForCode(event.code);
    const usage = keyboardCodeToHidUsage(event.code);
    const consumerUsage = keyboardCodeToConsumerUsage(event.code);
    if (!sc && usage === null && consumerUsage === null) {
      if (shouldPreventDefault) {
        event.preventDefault();
        event.stopPropagation();
      }
      return;
    }

    if (!event.repeat) {
      this.pressedCodes.add(event.code);
    }

    if (shouldPreventDefault) {
      event.preventDefault();
      event.stopPropagation();
    }

    const tsUs = toTimestampUs(event.timeStamp);
    if (!event.repeat && usage !== null) {
      this.queue.pushKeyHidUsage(tsUs, usage, true);
    }
    if (!event.repeat && consumerUsage !== null) {
      this.queue.pushHidUsage16(tsUs, 0x0c, consumerUsage, true);
    }
    if (sc) {
      pushSet2ScancodeSequence(this.queue, tsUs, sc, true);
    }
  };

  private readonly handleKeyUp = (event: KeyboardEvent): void => {
    if (!this.isCapturingKeyboard()) {
      return;
    }

    if (this.suppressedKeyUps.delete(event.code)) {
      // Swallow the matching keyup for a host-only chord so the guest does not observe a stray break.
      event.preventDefault();
      event.stopPropagation();
      return;
    }

    const shouldPreventDefault = shouldPreventDefaultForKeyboardEvent(event);

    const sc = ps2Set2ScancodeForCode(event.code);
    const usage = keyboardCodeToHidUsage(event.code);
    const consumerUsage = keyboardCodeToConsumerUsage(event.code);
    if (!sc && usage === null && consumerUsage === null) {
      if (shouldPreventDefault) {
        event.preventDefault();
        event.stopPropagation();
      }
      return;
    }

    this.pressedCodes.delete(event.code);

    if (shouldPreventDefault) {
      event.preventDefault();
      event.stopPropagation();
    }

    const tsUs = toTimestampUs(event.timeStamp);
    if (usage !== null) {
      this.queue.pushKeyHidUsage(tsUs, usage, false);
    }
    if (consumerUsage !== null) {
      this.queue.pushHidUsage16(tsUs, 0x0c, consumerUsage, false);
    }
    if (sc) {
      pushSet2ScancodeSequence(this.queue, tsUs, sc, false);
    }
  };

  private readonly handleMouseMove = (event: MouseEvent): void => {
    if (!this.pointerLock.isLocked) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    // Pointer lock movementX/Y are already deltas; scale and preserve fractions.
    this.mouseFracX += event.movementX * this.mouseSensitivity;
    this.mouseFracY += event.movementY * this.mouseSensitivity;

    const dx = this.takeWholeMouseDelta('x');
    const dy = this.takeWholeMouseDelta('y');

    if (dx === 0 && dy === 0) {
      return;
    }

    // PS/2 convention: positive Y is up (DOM is typically positive down).
    const tsUs = toTimestampUs(event.timeStamp);
    const dyPs2 = negateI32Saturating(dy | 0);
    this.queue.pushMouseMove(tsUs, dx, dyPs2);
  };

  private readonly handleMouseDown = (event: MouseEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }
    // When pointer lock is not active, only swallow clicks originating from the canvas itself.
    // (The listener is attached to `document` in capture phase, so without this check we'd
    // interfere with clicks on unrelated UI while the canvas still has focus.)
    if (!this.pointerLock.isLocked && event.target !== this.canvas) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const bit = buttonToMask(event.button);
    if (bit === 0) {
      return;
    }
    this.setMouseButtons(this.mouseButtons | bit, event.timeStamp);
  };

  private readonly handleMouseUp = (event: MouseEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }
    const onCanvas = event.target === this.canvas;
    const bit = buttonToMask(event.button);

    // When pointer lock is not active, ignore mouseup events that did not originate from the canvas
    // *unless* we are currently tracking that button as pressed (e.g. the user clicked in the VM,
    // dragged out of the canvas, and released elsewhere). This avoids "stuck buttons" in the guest
    // without interfering with unrelated page UI interactions.
    if (!this.pointerLock.isLocked && !onCanvas) {
      if (bit === 0 || (this.mouseButtons & bit) === 0) {
        return;
      }
    }

    event.preventDefault();
    event.stopPropagation();
    if (bit === 0) {
      return;
    }
    this.setMouseButtons(this.mouseButtons & ~bit, event.timeStamp);
  };

  private readonly handleAuxClick = (event: MouseEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }

    // `auxclick` is fired for non-primary buttons (middle/back/forward/etc.). Even if the
    // corresponding `mousedown`/`mouseup` were swallowed, this synthesized event would still bubble
    // and can trigger host-side behavior (e.g. autoscroll) or app-level listeners.
    if (!this.pointerLock.isLocked && event.target !== this.canvas) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();
  };

  private readonly handleWheel = (event: WheelEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }

    // Prevent page scroll while interacting with the VM.
    event.preventDefault();
    event.stopPropagation();

    this.wheelFrac += wheelDeltaToSteps(event.deltaY, event.deltaMode, { invert: true });
    this.wheelFracX += wheelDeltaToSteps(event.deltaX, event.deltaMode, { invert: false });

    const dz = this.takeWholeWheelDelta();
    const dx = this.takeWholeWheelDeltaX();
    if (dz === 0 && dx === 0) {
      return;
    }

    const tsUs = toTimestampUs(event.timeStamp);
    this.queue.pushMouseWheel(tsUs, dz, dx);
  };

  private readonly handleContextMenu = (event: Event): void => {
    if (this.isCapturingMouse()) {
      event.preventDefault();
      event.stopPropagation();
    }
  };

  private readonly handleTouchStart = (event: TouchEvent): void => {
    if (!this.enableTouchFallback) {
      return;
    }
    // Touch fallback is only active when pointer lock is *not* active.
    if (this.pointerLock.isLocked) {
      return;
    }
    if (!this.windowFocused || !this.pageVisible) {
      return;
    }

    // Prevent page scroll/zoom while interacting with the VM canvas.
    event.preventDefault();
    event.stopPropagation();

    this.canvas.focus();

    // Only track a single primary touch; ignore additional touches but still
    // preventDefault to avoid host-side gestures (scroll/zoom/back-swipe).
    if (this.touchActiveId !== null) {
      // Multi-touch gesture: disable tap-to-click for this session.
      this.touchHadMultiTouch = true;
      return;
    }

    const touch = touchListFirst(event.changedTouches);
    if (!touch) {
      return;
    }

    const id = touch.identifier;
    if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 0) {
      // Defensive: invalid touch identifiers can break `touchListFind` (NaN !== NaN), leading to
      // stuck sessions or synthesized taps. Ignore these events entirely.
      return;
    }

    const startX = touch.clientX;
    const startY = touch.clientY;
    if (!Number.isFinite(startX) || !Number.isFinite(startY)) {
      // Defensive: ignore malformed touches so they can't poison gesture state or synthesize a tap.
      return;
    }

    // Touch boundaries are a natural "capture session" boundary: drop any fractional remainder so
    // it cannot leak into a later session and cause a spurious pixel or wheel tick.
    this.resetAccumulatedMotion();

    this.touchActiveId = id;
    this.touchLastX = startX;
    this.touchLastY = startY;
    this.touchStartX = startX;
    this.touchStartY = startY;
    this.touchStartTimeStamp = event.timeStamp;
    this.touchMaxDistSq = 0;
    this.touchHadMultiTouch = event.touches.length > 1;
  };

  private readonly handleTouchMove = (event: TouchEvent): void => {
    if (!this.enableTouchFallback) {
      return;
    }
    if (this.pointerLock.isLocked) {
      return;
    }
    if (!this.windowFocused || !this.pageVisible) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    const id = this.touchActiveId;
    if (id === null) {
      return;
    }
    if (!Number.isSafeInteger(id) || id < 0) {
      this.clearTouchState();
      this.resetAccumulatedMotion();
      return;
    }

    const touch = touchListFind(event.changedTouches, id) ?? touchListFind(event.touches, id);
    if (!touch) {
      return;
    }

    if (event.touches.length > 1) {
      this.touchHadMultiTouch = true;
    }

    const x = touch.clientX;
    const y = touch.clientY;
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      // Malformed coordinate stream: cancel the session so it cannot synthesize a tap/click later.
      this.clearTouchState();
      this.resetAccumulatedMotion();
      return;
    }

    const dx0 = x - this.touchStartX;
    const dy0 = y - this.touchStartY;
    const distSq = dx0 * dx0 + dy0 * dy0;
    if (distSq > this.touchMaxDistSq) {
      this.touchMaxDistSq = distSq;
    }

    const dxRaw = x - this.touchLastX;
    const dyRaw = y - this.touchLastY;
    this.touchLastX = x;
    this.touchLastY = y;

    this.mouseFracX += dxRaw * this.touchSensitivity;
    this.mouseFracY += dyRaw * this.touchSensitivity;

    const dx = this.takeWholeMouseDelta("x");
    const dy = this.takeWholeMouseDelta("y");
    if (dx === 0 && dy === 0) {
      return;
    }

    const tsUs = toTimestampUs(event.timeStamp);
    // PS/2 convention: positive Y is up (DOM is typically positive down).
    const dyPs2 = negateI32Saturating(dy | 0);
    this.queue.pushMouseMove(tsUs, dx, dyPs2);
  };

  private handleTouchEndInternal(event: TouchEvent, { allowTap }: { allowTap: boolean }): void {
    if (!this.enableTouchFallback) {
      return;
    }
    if (this.pointerLock.isLocked) {
      return;
    }
    if (!this.windowFocused || !this.pageVisible) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    const id = this.touchActiveId;
    if (id === null) {
      return;
    }
    if (!Number.isSafeInteger(id) || id < 0) {
      this.clearTouchState();
      this.resetAccumulatedMotion();
      return;
    }

    if (event.touches.length > 1) {
      this.touchHadMultiTouch = true;
    }

    const ended = touchListFind(event.changedTouches, id);
    const stillActive = ended ? false : touchListFind(event.touches, id) !== null;
    if (stillActive) {
      // Another finger may have ended; keep tracking the primary one.
      return;
    }

    if (this.touchTapToClick && allowTap && !this.touchHadMultiTouch) {
      // 6px movement, 250ms duration. (Small enough to allow a little finger jitter.)
      const durationMs = event.timeStamp - this.touchStartTimeStamp;
      const distSq = this.touchMaxDistSq;
      const moved = Number.isFinite(distSq) ? distSq > 6 * 6 : true;
      const longPress = !Number.isFinite(durationMs) || durationMs < 0 || durationMs > 250;
      if (!moved && !longPress) {
        // Emit click as down at touch-start timestamp + up at touch-end timestamp.
        //
        // Avoid interfering with other sources (e.g. a physical mouse) that may already be holding
        // the left button.
        if ((this.mouseButtons & 1) === 0) {
          this.setMouseButtons(this.mouseButtons | 1, this.touchStartTimeStamp);
          this.setMouseButtons(this.mouseButtons & ~1, event.timeStamp);
        }
      }
    }

    this.clearTouchState();
    this.resetAccumulatedMotion();
  }

  private readonly handleTouchEnd = (event: TouchEvent): void => {
    this.handleTouchEndInternal(event, { allowTap: true });
  };

  private readonly handleTouchCancel = (event: TouchEvent): void => {
    this.handleTouchEndInternal(event, { allowTap: false });
  };

  private readonly handlePointerDown = (event: PointerEvent): void => {
    if (!this.enableTouchFallback) {
      return;
    }
    if (this.pointerLock.isLocked) {
      return;
    }
    if (!this.windowFocused || !this.pageVisible) {
      return;
    }

    // Only treat touch / stylus pointers as the "touch capture" fallback. Mouse pointers continue
    // to flow through the existing mouse + pointer lock path.
    if (event.pointerType !== "touch" && event.pointerType !== "pen") {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    // Attempt to focus so the keyboard path becomes active (best-effort).
    this.canvas.focus();

    const id = event.pointerId;
    if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 0) {
      return;
    }
    const x = event.clientX;
    const y = event.clientY;
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      // Defensive: ignore malformed pointer coordinates so they can't synthesize a tap/click.
      return;
    }

    if (this.touchPointers.size === 0) {
      this.touchPrimaryPointerId = id;
      this.touchStartX = x;
      this.touchStartY = y;
      this.touchStartTimeStamp = event.timeStamp;
      this.touchMaxDistSq = 0;
      this.touchHadMultiTouch = false;
      // Pointer/touch sessions are a natural "capture boundary". Drop any fractional remainder so
      // it cannot leak into a later session and cause a spurious pixel or wheel tick.
      this.resetAccumulatedMotion();
    } else {
      this.touchHadMultiTouch = true;
    }

    this.touchPointers.set(id, { x, y });

    try {
      (this.canvas as unknown as { setPointerCapture?: (id: number) => void }).setPointerCapture?.(id);
    } catch {
      // Ignore; pointer capture is best-effort.
    }
  };

  private readonly handlePointerMove = (event: PointerEvent): void => {
    if (!this.enableTouchFallback) {
      return;
    }
    if (this.pointerLock.isLocked) {
      return;
    }
    if (event.pointerType !== "touch" && event.pointerType !== "pen") {
      return;
    }

    const prev = this.touchPointers.get(event.pointerId);
    if (!prev) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    const x = event.clientX;
    const y = event.clientY;
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      // Malformed coordinate stream: cancel so it cannot synthesize a tap/click.
      this.cancelTouchCapture();
      this.clearTouchState();
      this.resetAccumulatedMotion();
      return;
    }

    // Track max distance for tap detection (primary pointer only).
    if (event.pointerId === this.touchPrimaryPointerId) {
      const dx0 = x - this.touchStartX;
      const dy0 = y - this.touchStartY;
      const distSq = dx0 * dx0 + dy0 * dy0;
      if (distSq > this.touchMaxDistSq) {
        this.touchMaxDistSq = distSq;
      }
    }

    if (this.touchPointers.size === 1) {
      const dxRaw = x - prev.x;
      const dyRaw = y - prev.y;
      prev.x = x;
      prev.y = y;

      this.mouseFracX += dxRaw * this.touchSensitivity;
      this.mouseFracY += dyRaw * this.touchSensitivity;

      const dx = this.takeWholeMouseDelta("x");
      const dy = this.takeWholeMouseDelta("y");
      if (dx === 0 && dy === 0) {
        return;
      }

      const tsUs = toTimestampUs(event.timeStamp);
      // PS/2 convention: positive Y is up (DOM is typically positive down).
      const dyPs2 = negateI32Saturating(dy | 0);
      this.queue.pushMouseMove(tsUs, dx, dyPs2);
      return;
    }

    // Two-finger gesture: translate average movement into wheel deltas.
    if (this.touchPointers.size === 2) {
      this.touchHadMultiTouch = true;

      let sumXBefore = 0;
      let sumYBefore = 0;
      for (const p of this.touchPointers.values()) {
        sumXBefore += p.x;
        sumYBefore += p.y;
      }

      prev.x = x;
      prev.y = y;

      let sumXAfter = 0;
      let sumYAfter = 0;
      for (const p of this.touchPointers.values()) {
        sumXAfter += p.x;
        sumYAfter += p.y;
      }

      const dxAvg = (sumXAfter - sumXBefore) / 2;
      const dyAvg = (sumYAfter - sumYBefore) / 2;
      // Match `wheelDeltaToSteps` pixel behavior: ~100px per wheel "click". Vertical is inverted so
      // positive is wheel up.
      this.wheelFrac += (-dyAvg) / 100;
      this.wheelFracX += dxAvg / 100;

      const dz = this.takeWholeWheelDelta();
      const dx = this.takeWholeWheelDeltaX();
      if (dz !== 0 || dx !== 0) {
        const tsUs = toTimestampUs(event.timeStamp);
        this.queue.pushMouseWheel(tsUs, dz, dx);
      }
      return;
    }

    // 3+ touches: just track coordinates and avoid generating guest input.
    this.touchHadMultiTouch = true;
    prev.x = x;
    prev.y = y;
  };

  private readonly handlePointerUp = (event: PointerEvent): void => {
    this.handlePointerEnd(event, { allowTap: true });
  };

  private readonly handlePointerCancel = (event: PointerEvent): void => {
    this.handlePointerEnd(event, { allowTap: false });
  };

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly ioWorker: InputBatchTarget,
    {
      mouseSensitivity = 1.0,
      touchSensitivity = mouseSensitivity,
      flushHz = 125,
      releasePointerLockChord,
      logCaptureLatency = false,
      recycleBuffers = true,
      enableGamepad = true,
      enableKeyboardLock = true,
      onBeforeSendBatch,
      gamepadDeadzone = 0.12,
      gamepadPollHz,
      enableTouchFallback = false,
      touchTapToClick = true,
    }: InputCaptureOptions = {}
  ) {
    // Be defensive: these values may come from user config and should not be allowed to create
    // pathological timer behavior (e.g. `setInterval(NaN)` which can behave like 0ms in browsers).
    const sanitizeFinite = (value: number, fallback: number): number => (Number.isFinite(value) ? value : fallback);
    const sanitizeHz = (value: number, fallback: number): number => {
      const v = sanitizeFinite(value, fallback);
      return v > 0 ? v : fallback;
    };

    // Keep defaults in one place so tests can assert behavior without duplicating magic numbers.
    const defaultFlushHz = 125;

    const sanitizedFlushHz = sanitizeHz(flushHz, defaultFlushHz);
    const sanitizedMouseSensitivity = sanitizeFinite(mouseSensitivity, 1.0);
    const sanitizedTouchSensitivity = sanitizeFinite(touchSensitivity, sanitizedMouseSensitivity);

    this.mouseSensitivity = sanitizedMouseSensitivity;
    this.touchSensitivity = sanitizedTouchSensitivity;
    this.flushHz = sanitizedFlushHz;
    this.releaseChord = releasePointerLockChord;
    this.logCaptureLatency = logCaptureLatency;
    this.recycleBuffers = recycleBuffers;
    this.enableKeyboardLock = enableKeyboardLock;
    this.enableTouchFallback = enableTouchFallback;
    this.touchTapToClick = touchTapToClick;
    this.onBeforeSendBatch = onBeforeSendBatch;
    this.flushOpts = { recycle: this.recycleBuffers, onBeforeSend: this.onBeforeSendBatch };
    this.flushOptsNoRecycle = { recycle: false, onBeforeSend: this.onBeforeSendBatch };

    const sanitizedDeadzone = Math.max(0, Math.min(1, sanitizeFinite(gamepadDeadzone, 0.12)));
    this.gamepad = enableGamepad ? new GamepadCapture({ deadzone: sanitizedDeadzone }) : null;
    const pollHzInput = gamepadPollHz ?? sanitizedFlushHz;
    const effectivePollHz = Math.max(1, Math.round(sanitizeHz(pollHzInput, sanitizedFlushHz)));
    this.gamepadPollIntervalMs = Math.max(1, Math.round(1000 / effectivePollHz));

    // Ensure the canvas can receive keyboard focus.
    if (this.canvas.tabIndex < 0) {
      this.canvas.tabIndex = 0;
    }

    this.pointerLock = new PointerLock(this.canvas, {
      onChange: this.handlePointerLockChange,
      onError: this.handlePointerLockError,
    });

    this.queue = new InputEventQueue(128, (byteLength) => this.takeRecycledBuffer(byteLength));
  }

  start(): void {
    if (this.flushTimer !== null) {
      return;
    }

    // `stop()` detaches document-level pointer lock listeners. Reattach them on
    // restart so `pointerLocked` state remains accurate across stop/start cycles.
    this.pointerLock.attach();

    // Refresh focus state in case the capture was stopped while the window was
    // blurred/hidden and restarted later without another focus event firing.
    this.windowFocused = typeof document !== "undefined" ? document.hasFocus() : true;
    this.pageVisible = typeof document !== "undefined" ? document.visibilityState !== "hidden" : true;
    this.hasFocus = typeof document !== "undefined" ? document.activeElement === this.canvas : false;

    // Optional: listen for recycled buffers from the worker.
    const workerWithEvents = this.ioWorker as unknown as MessageEventTarget;
    workerWithEvents.addEventListener?.("message", this.handleWorkerMessage);

    this.canvas.addEventListener('click', this.handleClick);
    this.canvas.addEventListener('focus', this.handleFocus);
    this.canvas.addEventListener('blur', this.handleBlur);

    // Use capture phase to get first shot at keys before the browser scrolls.
    window.addEventListener('keydown', this.handleKeyDown, { capture: true });
    window.addEventListener('keyup', this.handleKeyUp, { capture: true });
    window.addEventListener("blur", this.handleWindowBlur);
    window.addEventListener("focus", this.handleWindowFocus);
    document.addEventListener("visibilitychange", this.handleVisibilityChange);

    // Mouse events should be observed at the document level while pointer locked.
    document.addEventListener('mousemove', this.handleMouseMove, { capture: true });
    document.addEventListener('mousedown', this.handleMouseDown, { capture: true });
    document.addEventListener('mouseup', this.handleMouseUp, { capture: true });
    document.addEventListener('auxclick', this.handleAuxClick, { capture: true });

    // Wheel must be non-passive so we can preventDefault.
    this.canvas.addEventListener('wheel', this.handleWheel, { passive: false });
    this.canvas.addEventListener('contextmenu', this.handleContextMenu);

    if (this.enableTouchFallback) {
      // Pointer Events rely on CSS `touch-action` to control default panning/zooming behavior.
      // Set it to `none` while capture is active to ensure touch drag is delivered as pointer/touch
      // events and does not scroll the page.
      const style = this.canvas.style as unknown as { touchAction?: unknown } | undefined;
      if (style && typeof style.touchAction === "string") {
        this.prevCanvasTouchAction = style.touchAction;
        style.touchAction = "none";
      }

      // Touch fallback. Prefer Pointer Events when available (better multi-touch semantics), but
      // fall back to Touch Events for older/restricted environments.
      if (typeof window !== "undefined" && typeof (window as unknown as { PointerEvent?: unknown }).PointerEvent !== "undefined") {
        this.canvas.addEventListener("pointerdown", this.handlePointerDown as EventListener, { passive: false });
        this.canvas.addEventListener("pointermove", this.handlePointerMove as EventListener, { passive: false });
        this.canvas.addEventListener("pointerup", this.handlePointerUp as EventListener, { passive: false });
        this.canvas.addEventListener("pointercancel", this.handlePointerCancel as EventListener, { passive: false });
      } else {
        this.canvas.addEventListener("touchstart", this.handleTouchStart as EventListener, { passive: false });
        this.canvas.addEventListener("touchmove", this.handleTouchMove as EventListener, { passive: false });
        this.canvas.addEventListener("touchend", this.handleTouchEnd as EventListener, { passive: false });
        this.canvas.addEventListener("touchcancel", this.handleTouchCancel as EventListener, { passive: false });
      }
    }

    const intervalMs = Math.max(1, Math.round(1000 / this.flushHz));
    this.flushTimer = window.setInterval(() => this.flushNow(), intervalMs);
    unrefBestEffort(this.flushTimer);
  }

  stop(): void {
    if (this.flushTimer === null) {
      return;
    }

    if (this.enableTouchFallback) {
      const style = this.canvas.style as unknown as { touchAction?: unknown } | undefined;
      if (style && typeof style.touchAction === "string" && this.prevCanvasTouchAction !== null) {
        style.touchAction = this.prevCanvasTouchAction;
      }
      this.prevCanvasTouchAction = null;
    }

    this.hasFocus = false;
    this.releaseKeyboardLock();
    this.suppressedKeyUps.clear();
    this.clearTouchState();
    this.resetAccumulatedMotion();
    this.cancelTouchCapture();

    window.clearInterval(this.flushTimer);
    this.flushTimer = null;

    // Flush a final "all released" state before detaching so the guest cannot get
    // stuck with latched inputs if capture is stopped while keys/buttons are held.
    this.pointerLock.exit();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    try {
      this.queue.flush(this.ioWorker, this.flushOptsNoRecycle);
    } catch {
      // `InputCapture.stop()` must be exception-safe: callers may race with worker shutdown and
      // attempt to stop capture after the target worker is already terminated. In that case,
      // `postMessage` throws and we still need to detach all DOM listeners to avoid leaving the
      // page in a partially-capturing state.
    }

    const workerWithEvents = this.ioWorker as unknown as MessageEventTarget;
    workerWithEvents.removeEventListener?.("message", this.handleWorkerMessage);

    this.canvas.removeEventListener('click', this.handleClick);
    this.canvas.removeEventListener('focus', this.handleFocus);
    this.canvas.removeEventListener('blur', this.handleBlur);

    window.removeEventListener('keydown', this.handleKeyDown, { capture: true } as AddEventListenerOptions);
    window.removeEventListener('keyup', this.handleKeyUp, { capture: true } as AddEventListenerOptions);
    window.removeEventListener("blur", this.handleWindowBlur);
    window.removeEventListener("focus", this.handleWindowFocus);
    document.removeEventListener("visibilitychange", this.handleVisibilityChange);

    document.removeEventListener('mousemove', this.handleMouseMove, { capture: true } as AddEventListenerOptions);
    document.removeEventListener('mousedown', this.handleMouseDown, { capture: true } as AddEventListenerOptions);
    document.removeEventListener('mouseup', this.handleMouseUp, { capture: true } as AddEventListenerOptions);
    document.removeEventListener('auxclick', this.handleAuxClick, { capture: true } as AddEventListenerOptions);

    this.canvas.removeEventListener('wheel', this.handleWheel as EventListener);
    this.canvas.removeEventListener('contextmenu', this.handleContextMenu);

    if (this.enableTouchFallback) {
      if (typeof window !== "undefined" && typeof (window as unknown as { PointerEvent?: unknown }).PointerEvent !== "undefined") {
        this.canvas.removeEventListener("pointerdown", this.handlePointerDown as EventListener);
        this.canvas.removeEventListener("pointermove", this.handlePointerMove as EventListener);
        this.canvas.removeEventListener("pointerup", this.handlePointerUp as EventListener);
        this.canvas.removeEventListener("pointercancel", this.handlePointerCancel as EventListener);
      } else {
        this.canvas.removeEventListener("touchstart", this.handleTouchStart as EventListener);
        this.canvas.removeEventListener("touchmove", this.handleTouchMove as EventListener);
        this.canvas.removeEventListener("touchend", this.handleTouchEnd as EventListener);
        this.canvas.removeEventListener("touchcancel", this.handleTouchCancel as EventListener);
      }
    }

    this.pointerLock.dispose();
  }

  flushNow(): void {
    this.pollGamepad();
    const hadEvents = this.queue.size > 0;
    let latencyUs: number | null;
    try {
      latencyUs = this.queue.flush(this.ioWorker, this.flushOpts);
    } catch {
      // The target worker may have been terminated. Stop capture so we don't keep
      // swallowing host input while being unable to deliver it to the guest.
      try {
        this.stop();
      } catch {
        // ignore
      }
      return;
    }
    if (latencyUs === null) {
      // If we had input events but could not deliver them (e.g. worker terminated and
      // `postMessage` threw), stop capture so we don't keep swallowing host input.
      if (hadEvents) {
        try {
          this.stop();
        } catch {
          // ignore
        }
      }
      return;
    }
    if (!this.logCaptureLatency) {
      return;
    }

    this.latencyLogCount++;
    this.latencyLogSumUs += latencyUs;
    if (latencyUs > this.latencyLogMaxUs) {
      this.latencyLogMaxUs = latencyUs;
    }

    const nowMs = performance.now();
    if (nowMs - this.latencyLogLastMs < 1000) {
      return;
    }

    const avgUs = Math.round(this.latencyLogSumUs / Math.max(1, this.latencyLogCount));
    console.debug(
      `[aero-input] capture→postMessage latency avg=${(avgUs / 1000).toFixed(2)}ms max=${(
        this.latencyLogMaxUs / 1000
      ).toFixed(2)}ms samples=${this.latencyLogCount}`
    );

    this.latencyLogLastMs = nowMs;
    this.latencyLogCount = 0;
    this.latencyLogSumUs = 0;
    this.latencyLogMaxUs = 0;
  }

  get pointerLocked(): boolean {
    return this.pointerLock.isLocked;
  }

  private isCapturingKeyboard(): boolean {
    return this.windowFocused && this.pageVisible && (this.hasFocus || this.pointerLock.isLocked);
  }

  private isCapturingMouse(): boolean {
    // If pointer lock is active, we always capture. Otherwise require focus to avoid
    // eating wheel events while the user scrolls elsewhere on the page.
    return this.windowFocused && this.pageVisible && (this.pointerLock.isLocked || this.hasFocus);
  }

  private releaseAllKeys(): void {
    if (this.pressedCodes.size === 0) {
      return;
    }

    const nowUs = toTimestampUs(performance.now());
    for (const code of this.pressedCodes) {
      const sc = ps2Set2ScancodeForCode(code);
      if (sc) {
        pushSet2ScancodeSequence(this.queue, nowUs, sc, false);
      }
      const usage = keyboardCodeToHidUsage(code);
      if (usage !== null) {
        this.queue.pushKeyHidUsage(nowUs, usage, false);
      }
      const consumerUsage = keyboardCodeToConsumerUsage(code);
      if (consumerUsage !== null) {
        this.queue.pushHidUsage16(nowUs, 0x0c, consumerUsage, false);
      }
    }
    this.pressedCodes.clear();
  }

  private setMouseButtons(next: number, timeStamp?: number): void {
    if (next === this.mouseButtons) {
      return;
    }
    this.mouseButtons = next;
    const tsUs = toTimestampUs(timeStamp ?? performance.now());
    this.queue.pushMouseButtons(tsUs, next);
  }

  private takeWholeMouseDelta(axis: 'x' | 'y'): number {
    if (axis === 'x') {
      if (!Number.isFinite(this.mouseFracX)) {
        // Defensive: avoid poisoning the fractional accumulator (e.g. if a browser/event shim
        // produces NaN/Infinity deltas). Reset to a clean state so subsequent motion works.
        this.mouseFracX = 0;
        return 0;
      }
      const whole = this.mouseFracX < 0 ? Math.ceil(this.mouseFracX) : Math.floor(this.mouseFracX);
      if (whole > I32_MAX) {
        this.mouseFracX = 0;
        return I32_MAX;
      }
      if (whole < I32_MIN) {
        this.mouseFracX = 0;
        return I32_MIN;
      }
      this.mouseFracX -= whole;
      return whole | 0;
    }

    if (!Number.isFinite(this.mouseFracY)) {
      this.mouseFracY = 0;
      return 0;
    }
    const whole = this.mouseFracY < 0 ? Math.ceil(this.mouseFracY) : Math.floor(this.mouseFracY);
    if (whole > I32_MAX) {
      this.mouseFracY = 0;
      return I32_MAX;
    }
    if (whole < I32_MIN) {
      this.mouseFracY = 0;
      return I32_MIN;
    }
    this.mouseFracY -= whole;
    return whole | 0;
  }

  private takeRecycledBuffer(byteLength: number): ArrayBuffer {
    if (this.recycleBuffers) {
      const bucket = this.recycledBuffersBySize.get(byteLength);
      while (bucket && bucket.length > 0) {
        const buf = bucket.pop();
        if (buf && buf.byteLength === byteLength) {
          if (bucket.length === 0) {
            this.recycledBuffersBySize.delete(byteLength);
          }
          return buf;
        }
      }
      if (bucket && bucket.length === 0) {
        this.recycledBuffersBySize.delete(byteLength);
      }
    }
    return new ArrayBuffer(byteLength);
  }

  private resetAccumulatedMotion(): void {
    this.mouseFracX = 0;
    this.mouseFracY = 0;
    this.wheelFrac = 0;
    this.wheelFracX = 0;
  }

  private clearTouchState(): void {
    this.touchActiveId = null;
    this.touchLastX = 0;
    this.touchLastY = 0;
    this.touchStartX = 0;
    this.touchStartY = 0;
    this.touchStartTimeStamp = 0;
    this.touchMaxDistSq = 0;
    this.touchHadMultiTouch = false;
  }

  private cancelTouchCapture(): void {
    if (this.touchPointers.size === 0) {
      return;
    }
    this.touchPointers.clear();
    this.touchPrimaryPointerId = null;
    this.touchHadMultiTouch = false;
    this.touchMaxDistSq = 0;
  }

  private handlePointerEnd(event: PointerEvent, { allowTap }: { allowTap: boolean }): void {
    if (!this.enableTouchFallback) {
      return;
    }
    if (this.pointerLock.isLocked) {
      return;
    }
    if (event.pointerType !== "touch" && event.pointerType !== "pen") {
      return;
    }

    if (!this.touchPointers.has(event.pointerId)) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    this.touchPointers.delete(event.pointerId);

    // Only treat the interaction as a tap if:
    // - tap-to-click is enabled,
    // - it was a single-touch session (no multi-touch),
    // - it did not move beyond a small threshold, and
    // - it ended on the primary pointer.
    if (
      this.touchTapToClick &&
      allowTap &&
      this.touchPointers.size === 0 &&
      !this.touchHadMultiTouch &&
      event.pointerId === this.touchPrimaryPointerId
    ) {
      // 6px movement, 250ms duration. (Small enough to allow a little finger jitter.)
      const durationMs = event.timeStamp - this.touchStartTimeStamp;
      const distSq = this.touchMaxDistSq;
      const moved = Number.isFinite(distSq) ? distSq > 6 * 6 : true;
      const longPress = !Number.isFinite(durationMs) || durationMs < 0 || durationMs > 250;
      if (!moved && !longPress) {
        // Emit click as down at touch-start timestamp + up at touch-end timestamp.
        //
        // Avoid interfering with other sources (e.g. a physical mouse) that may already be holding
        // the left button.
        if ((this.mouseButtons & 1) === 0) {
          this.setMouseButtons(this.mouseButtons | 1, this.touchStartTimeStamp);
          this.setMouseButtons(this.mouseButtons & ~1, event.timeStamp);
        }
      }
    }

    if (this.touchPointers.size === 0) {
      this.touchPrimaryPointerId = null;
      this.touchHadMultiTouch = false;
      this.touchMaxDistSq = 0;
      this.resetAccumulatedMotion();
    }

    try {
      (this.canvas as unknown as { releasePointerCapture?: (id: number) => void }).releasePointerCapture?.(event.pointerId);
    } catch {
      // Ignore.
    }
  }

  private takeWholeWheelDelta(): number {
    if (!Number.isFinite(this.wheelFrac)) {
      this.wheelFrac = 0;
      return 0;
    }
    const whole = this.wheelFrac < 0 ? Math.ceil(this.wheelFrac) : Math.floor(this.wheelFrac);
    if (whole > I32_MAX) {
      this.wheelFrac = 0;
      return I32_MAX;
    }
    if (whole < I32_MIN) {
      this.wheelFrac = 0;
      return I32_MIN;
    }
    this.wheelFrac -= whole;
    return whole | 0;
  }

  private takeWholeWheelDeltaX(): number {
    if (!Number.isFinite(this.wheelFracX)) {
      this.wheelFracX = 0;
      return 0;
    }
    const whole = this.wheelFracX < 0 ? Math.ceil(this.wheelFracX) : Math.floor(this.wheelFracX);
    if (whole > I32_MAX) {
      this.wheelFracX = 0;
      return I32_MAX;
    }
    if (whole < I32_MIN) {
      this.wheelFracX = 0;
      return I32_MIN;
    }
    this.wheelFracX -= whole;
    return whole | 0;
  }

  private pollGamepad(): void {
    if (!this.gamepad) {
      return;
    }

    if (!this.isCapturingKeyboard()) {
      return;
    }

    const nowMs = performance.now();
    if (nowMs - this.gamepadLastPollMs < this.gamepadPollIntervalMs) {
      return;
    }
    this.gamepadLastPollMs = nowMs;

    this.gamepad.poll(this.queue, toTimestampUs(nowMs), { active: true });
  }

  private shouldLockKeyboard(): boolean {
    // Only attempt keyboard lock when capture is active and the user explicitly initiated capture
    // via a gesture (click). This improves delivery of browser-reserved keys (e.g. Escape, function
    // keys) to the guest without unexpectedly changing global keyboard behavior.
    return this.enableKeyboardLock && this.keyboardLockRequested && this.isCapturingKeyboard();
  }

  private updateKeyboardLock(): void {
    if (!this.enableKeyboardLock) {
      return;
    }

    const keyboard = getNavigatorKeyboard();
    if (!keyboard) {
      return;
    }

    if (!this.shouldLockKeyboard()) {
      if (this.keyboardLockActive) {
        this.releaseKeyboardLock();
      }
      return;
    }

    if (this.keyboardLockActive) {
      return;
    }

    if (!this.keyboardLockRequested) {
      return;
    }

    this.keyboardLockActive = true;
    const seq = ++this.keyboardLockSeq;

    lockNavigatorKeyboard(keyboard, DEFAULT_KEYBOARD_LOCK_CODES)
      .then(() => {
        // If capture ended while the lock request was in-flight, ensure the keyboard is unlocked.
        if (this.keyboardLockSeq !== seq || !this.shouldLockKeyboard()) {
          safeUnlockNavigatorKeyboard(keyboard);
          this.keyboardLockActive = false;
        }
      })
      .catch((err) => {
        // Ignore failures: capture should continue even if the browser rejects the request.
        if (this.keyboardLockSeq !== seq) {
          return;
        }
        this.keyboardLockActive = false;
        console.debug(`[aero-input] navigator.keyboard.lock() failed: ${formatError(err)}`);
      });
  }

  private releaseKeyboardLock(): void {
    this.keyboardLockRequested = false;

    const keyboard = getNavigatorKeyboard();
    if (!keyboard) {
      this.keyboardLockActive = false;
      return;
    }

    if (!this.keyboardLockActive) {
      return;
    }

    // Invalidate any in-flight lock promise handlers.
    this.keyboardLockSeq += 1;
    this.keyboardLockActive = false;
    safeUnlockNavigatorKeyboard(keyboard);
  }
}

type MessageEventTarget = {
  addEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
  removeEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
};

type NavigatorKeyboardLockApi = {
  lock: (keyCodes?: readonly string[]) => Promise<void>;
  unlock?: () => void;
};

const DEFAULT_KEYBOARD_LOCK_CODES: readonly string[] = [
  "Escape",
  "F1",
  "F2",
  "F3",
  "F4",
  "F5",
  "F6",
  "F7",
  "F8",
  "F9",
  "F10",
  "F11",
  "F12",
];

function getNavigatorKeyboard(): NavigatorKeyboardLockApi | null {
  if (typeof navigator === "undefined") {
    return null;
  }
  const keyboard = (navigator as unknown as { keyboard?: unknown }).keyboard as Partial<NavigatorKeyboardLockApi> | undefined;
  if (!keyboard || typeof keyboard.lock !== "function") {
    return null;
  }
  return keyboard as NavigatorKeyboardLockApi;
}

function lockNavigatorKeyboard(
  keyboard: NavigatorKeyboardLockApi,
  keyCodes: readonly string[]
): Promise<void> {
  // Some implementations (or older typings) may be picky about the argument. Prefer an explicit
  // list of the keys we care about, but fall back to calling with no arguments if needed.
  // Some browsers reject (rather than throw) when the argument is unsupported, so handle both.
  const callWithoutArgs = (): Promise<void> => {
    try {
      return keyboard.lock();
    } catch (err) {
      return Promise.reject(err);
    }
  };
  try {
    return keyboard.lock(keyCodes).catch((err) => {
      if (err instanceof TypeError) {
        return callWithoutArgs();
      }
      return Promise.reject(err);
    });
  } catch (err) {
    if (err instanceof TypeError) {
      return callWithoutArgs();
    }
    return Promise.reject(err);
  }
}

function safeUnlockNavigatorKeyboard(keyboard: NavigatorKeyboardLockApi): void {
  if (typeof keyboard.unlock !== "function") {
    return;
  }
  try {
    keyboard.unlock();
  } catch (err) {
    console.debug(`[aero-input] navigator.keyboard.unlock() failed: ${formatError(err)}`);
  }
}

function formatError(err: unknown): string {
  return formatOneLineError(err, 256);
}

function chordMatches(event: KeyboardEvent, chord: PointerLockReleaseChord): boolean {
  if (event.code !== chord.code) {
    return false;
  }
  if ((chord.ctrlKey ?? false) !== event.ctrlKey) {
    return false;
  }
  if ((chord.altKey ?? false) !== event.altKey) {
    return false;
  }
  if ((chord.shiftKey ?? false) !== event.shiftKey) {
    return false;
  }
  if ((chord.metaKey ?? false) !== event.metaKey) {
    return false;
  }
  return true;
}

function toTimestampUs(timeStamp: number): number {
  // `timeStamp` is a DOMHighResTimeStamp (ms, fractional). Convert to a u32 μs
  // timeline for cheap transport. Wraparound at ~71 minutes is OK as long as
  // consumers do unsigned delta arithmetic.
  return Math.round(timeStamp * 1000) >>> 0;
}

function pushSet2ScancodeSequence(
  queue: InputEventQueue,
  timestampUs: number,
  sc: Ps2Set2Scancode,
  pressed: boolean
): void {
  if (sc.kind === "sequence") {
    const bytes = pressed ? sc.make : sc.break;
    pushPackedBytes(queue, timestampUs, bytes);
    return;
  }

  const make = sc.make & 0xff;
  const extended = sc.extended;

  if (pressed) {
    if (extended) {
      queue.pushKeyScancode(timestampUs, 0xe0 | (make << 8), 2);
    } else {
      queue.pushKeyScancode(timestampUs, make, 1);
    }
    return;
  }

  if (extended) {
    queue.pushKeyScancode(timestampUs, 0xe0 | (0xf0 << 8) | (make << 16), 3);
  } else {
    queue.pushKeyScancode(timestampUs, 0xf0 | (make << 8), 2);
  }
}

function pushPackedBytes(queue: InputEventQueue, timestampUs: number, bytes: readonly number[]): void {
  for (let i = 0; i < bytes.length; i += 4) {
    const len = Math.min(4, bytes.length - i);
    const b0 = bytes[i]! & 0xff;
    const b1 = len > 1 ? bytes[i + 1]! & 0xff : 0;
    const b2 = len > 2 ? bytes[i + 2]! & 0xff : 0;
    const b3 = len > 3 ? bytes[i + 3]! & 0xff : 0;
    const packed = b0 | (b1 << 8) | (b2 << 16) | (b3 << 24);
    queue.pushKeyScancode(timestampUs, packed, len);
  }
}

function buttonToMask(button: number): number {
  // Track the DOM `MouseEvent.buttons` bitfield for up to 8 mouse buttons.
  //
  // DOM `MouseEvent.button` uses this numbering:
  // - 0 primary/left
  // - 1 auxiliary/middle
  // - 2 secondary/right
  // - 3 back
  // - 4 forward
  //
  // Some browsers/devices may surface additional buttons as 5/6/7; map those to bits 5..7 so
  // virtio-input can forward them as BTN_FORWARD/BTN_BACK/BTN_TASK (Linux input ABI).
  //
  // Note: the PS/2 mouse backend only transmits back/forward when the guest has
  // enabled a 5-button PS/2 mouse variant (IntelliMouse Explorer, device ID 0x04).
  // Otherwise these bits are ignored by the PS/2 encoder, but virtio-input and USB
  // HID backends can expose them unconditionally.
  switch (button) {
    case 0:
      return 1;
    case 2:
      return 2;
    case 1:
      return 4;
    case 3:
      return 8;
    case 4:
      return 16;
    case 5:
      return 32;
    case 6:
      return 64;
    case 7:
      return 128;
    default:
      return 0;
  }
}

function wheelDeltaToSteps(rawDelta: unknown, rawDeltaMode: unknown, opts: { invert: boolean }): number {
  // Preserve fractional deltas (trackpads, high-resolution wheels) by allowing callers to
  // accumulate and quantize later.
  let delta = typeof rawDelta === "number" ? rawDelta : 0;
  if (!Number.isFinite(delta)) {
    delta = 0;
  }

  const deltaMode = typeof rawDeltaMode === "number" ? rawDeltaMode : 0;
  switch (deltaMode) {
    case 0: // WheelEvent.DOM_DELTA_PIXEL
      delta /= 100;
      break;
    case 1: // WheelEvent.DOM_DELTA_LINE
      // Leave as-is.
      break;
    case 2: // WheelEvent.DOM_DELTA_PAGE
      delta *= 3;
      break;
  }

  if (opts.invert) {
    // DOM: deltaY > 0 is scroll down; input backends typically treat positive as wheel up.
    delta = -delta;
  }
  return delta;
}

function touchListFirst(list: TouchList | null | undefined): Touch | null {
  if (!list || list.length === 0) {
    return null;
  }
  return list[0] ?? null;
}

function touchListFind(list: TouchList | null | undefined, id: number): Touch | null {
  if (!list) {
    return null;
  }
  // Use a simple loop to avoid allocations on the hot path.
  for (let i = 0; i < list.length; i++) {
    const t = list[i];
    if (t && t.identifier === id) {
      return t;
    }
  }
  return null;
}
