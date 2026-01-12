import { InputEventQueue, type InputBatchRecycleMessage, type InputBatchTarget } from "./event_queue";
import { GamepadCapture } from "./gamepad";
import { PointerLock } from "./pointer_lock";
import { keyboardCodeToHidUsage } from "./hid_usage";
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
   * How often to flush input to the I/O worker.
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
   * If enabled, request that the I/O worker transfers input batch buffers back
   * for reuse. This avoids allocating a new ArrayBuffer per flush.
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
   * Analog stick deadzone in normalized units ([0, 1]).
   */
  gamepadDeadzone?: number;
  /**
   * How often to poll the Gamepad API (max: `flushHz`).
   */
  gamepadPollHz?: number;
}

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
  private readonly releaseChord?: PointerLockReleaseChord;
  private readonly logCaptureLatency: boolean;
  private readonly recycleBuffers: boolean;

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
    const bucket = this.recycledBuffersBySize.get(size);
    if (bucket) {
      bucket.push(data.buffer);
    } else {
      this.recycledBuffersBySize.set(size, [data.buffer]);
    }
  };

  private flushTimer: number | null = null;

  private hasFocus = false;
  private windowFocused = typeof document !== "undefined" ? document.hasFocus() : true;
  private pageVisible = typeof document !== "undefined" ? document.visibilityState !== "hidden" : true;

  private mouseButtons = 0;
  private mouseFracX = 0;
  private mouseFracY = 0;
  private wheelFrac = 0;

  private latencyLogLastMs = 0;
  private latencyLogCount = 0;
  private latencyLogSumUs = 0;
  private latencyLogMaxUs = 0;

  private readonly handlePointerLockChange = (locked: boolean): void => {
    // If pointer lock exits while the canvas is not focused, we will stop
    // capturing keyboard/mouse events, which can leave the guest with latched
    // input state. Emit a best-effort "all released" snapshot immediately.
    if (locked || this.hasFocus) {
      return;
    }

    const nowUs = toTimestampUs(performance.now());
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, nowUs);
    this.queue.flush(this.ioWorker, { recycle: this.recycleBuffers });
  };

  private readonly handleClick = (): void => {
    this.canvas.focus();
    this.pointerLock.request();
  };

  private readonly handleFocus = (): void => {
    this.hasFocus = true;
  };

  private readonly handleBlur = (): void => {
    this.hasFocus = false;
    this.pointerLock.exit();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    // Flush immediately; timers may be throttled in the background and we don't
    // want the guest to observe "stuck" inputs.
    this.queue.flush(this.ioWorker, { recycle: this.recycleBuffers });
  };

  private readonly handleWindowBlur = (): void => {
    this.windowFocused = false;
    this.pointerLock.exit();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    this.queue.flush(this.ioWorker, { recycle: this.recycleBuffers });
  };

  private readonly handleWindowFocus = (): void => {
    this.windowFocused = true;
  };

  private readonly handleVisibilityChange = (): void => {
    this.pageVisible = document.visibilityState !== "hidden";
    if (this.pageVisible) {
      return;
    }

    this.pointerLock.exit();
    this.suppressedKeyUps.clear();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    this.queue.flush(this.ioWorker, { recycle: this.recycleBuffers });
  };

  private readonly handleKeyDown = (event: KeyboardEvent): void => {
    if (!this.isCapturingKeyboard()) {
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
    if (!sc && usage === null) {
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
    if (!sc && usage === null) {
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
    this.queue.pushMouseMove(tsUs, dx, -dy);
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
    if (!this.pointerLock.isLocked && event.target !== this.canvas) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const bit = buttonToMask(event.button);
    if (bit === 0) {
      return;
    }
    this.setMouseButtons(this.mouseButtons & ~bit, event.timeStamp);
  };

  private readonly handleWheel = (event: WheelEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }

    // Prevent page scroll while interacting with the VM.
    event.preventDefault();
    event.stopPropagation();

    this.wheelFrac += wheelEventToDeltaSteps(event);
    const dz = this.takeWholeWheelDelta();
    if (dz === 0) {
      return;
    }

    const tsUs = toTimestampUs(event.timeStamp);
    this.queue.pushMouseWheel(tsUs, dz);
  };

  private readonly handleContextMenu = (event: Event): void => {
    if (this.isCapturingMouse()) {
      event.preventDefault();
      event.stopPropagation();
    }
  };

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly ioWorker: InputBatchTarget,
    {
      mouseSensitivity = 1.0,
      flushHz = 125,
      releasePointerLockChord,
      logCaptureLatency = false,
      recycleBuffers = true,
      enableGamepad = true,
      gamepadDeadzone = 0.12,
      gamepadPollHz,
    }: InputCaptureOptions = {}
  ) {
    this.mouseSensitivity = mouseSensitivity;
    this.flushHz = flushHz;
    this.releaseChord = releasePointerLockChord;
    this.logCaptureLatency = logCaptureLatency;
    this.recycleBuffers = recycleBuffers;

    this.gamepad = enableGamepad ? new GamepadCapture({ deadzone: gamepadDeadzone }) : null;
    const effectivePollHz = Math.max(1, Math.round(gamepadPollHz ?? flushHz));
    this.gamepadPollIntervalMs = Math.max(1, Math.round(1000 / effectivePollHz));

    // Ensure the canvas can receive keyboard focus.
    if (this.canvas.tabIndex < 0) {
      this.canvas.tabIndex = 0;
    }

    this.pointerLock = new PointerLock(this.canvas, { onChange: this.handlePointerLockChange });

    this.queue = new InputEventQueue(128, (byteLength) => this.takeRecycledBuffer(byteLength));
  }

  start(): void {
    if (this.flushTimer !== null) {
      return;
    }

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

    // Wheel must be non-passive so we can preventDefault.
    this.canvas.addEventListener('wheel', this.handleWheel, { passive: false });
    this.canvas.addEventListener('contextmenu', this.handleContextMenu);

    const intervalMs = Math.max(1, Math.round(1000 / this.flushHz));
    this.flushTimer = window.setInterval(() => this.flushNow(), intervalMs);
    (this.flushTimer as unknown as { unref?: () => void }).unref?.();
  }

  stop(): void {
    if (this.flushTimer === null) {
      return;
    }

    this.hasFocus = false;
    this.suppressedKeyUps.clear();

    window.clearInterval(this.flushTimer);
    this.flushTimer = null;

    // Flush a final "all released" state before detaching so the guest cannot get
    // stuck with latched inputs if capture is stopped while keys/buttons are held.
    this.pointerLock.exit();
    this.releaseAllKeys();
    this.setMouseButtons(0);
    this.gamepad?.emitNeutral(this.queue, toTimestampUs(performance.now()));
    this.queue.flush(this.ioWorker, { recycle: false });

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

    this.canvas.removeEventListener('wheel', this.handleWheel as EventListener);
    this.canvas.removeEventListener('contextmenu', this.handleContextMenu);

    this.pointerLock.dispose();
  }

  flushNow(): void {
    this.pollGamepad();
    const latencyUs = this.queue.flush(this.ioWorker, { recycle: this.recycleBuffers });
    if (latencyUs === null || !this.logCaptureLatency) {
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
      const whole = this.mouseFracX < 0 ? Math.ceil(this.mouseFracX) : Math.floor(this.mouseFracX);
      this.mouseFracX -= whole;
      return whole | 0;
    }

    const whole = this.mouseFracY < 0 ? Math.ceil(this.mouseFracY) : Math.floor(this.mouseFracY);
    this.mouseFracY -= whole;
    return whole | 0;
  }

  private takeRecycledBuffer(byteLength: number): ArrayBuffer {
    if (this.recycleBuffers) {
      const bucket = this.recycledBuffersBySize.get(byteLength);
      const buf = bucket?.pop();
      if (buf) {
        return buf;
      }
    }
    return new ArrayBuffer(byteLength);
  }

  private takeWholeWheelDelta(): number {
    const whole = this.wheelFrac < 0 ? Math.ceil(this.wheelFrac) : Math.floor(this.wheelFrac);
    this.wheelFrac -= whole;
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
}

type MessageEventTarget = {
  addEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
  removeEventListener?: (type: "message", listener: (ev: MessageEvent<unknown>) => void) => void;
};

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
  // Track the DOM `MouseEvent.buttons` bitfield for the common 5 buttons:
  // - left/right/middle (classic)
  // - back/forward (aka side/extra buttons)
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
    default:
      return 0;
  }
}

function wheelEventToDeltaSteps(event: WheelEvent): number {
  // Preserve fractional deltas (trackpads, high-resolution wheels) by allowing callers to
  // accumulate and quantize later.
  let delta = event.deltaY;
  switch (event.deltaMode) {
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
  // DOM: deltaY > 0 is scroll down; PS/2: positive is wheel up.
  return -delta;
}
