import { InputEventQueue, type InputBatchTarget } from "./event_queue";
import { PointerLock } from "./pointer_lock";
import {
  shouldPreventDefaultForKeyboardEvent,
  translateCodeToSet2MakeCode
} from "./scancode";

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
}

export class InputCapture {
  private readonly queue = new InputEventQueue();
  private readonly pointerLock: PointerLock;
  private readonly pressedCodes = new Set<string>();

  private readonly flushHz: number;
  private readonly mouseSensitivity: number;
  private readonly releaseChord?: PointerLockReleaseChord;
  private readonly logCaptureLatency: boolean;

  private flushTimer: number | null = null;

  private hasFocus = false;

  private mouseButtons = 0;
  private mouseFracX = 0;
  private mouseFracY = 0;

  private latencyLogLastMs = 0;
  private latencyLogCount = 0;
  private latencyLogSumUs = 0;
  private latencyLogMaxUs = 0;

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
    this.releaseAllKeys();
    this.setMouseButtons(0);
  };

  private readonly handleKeyDown = (event: KeyboardEvent): void => {
    if (!this.isCapturingKeyboard()) {
      return;
    }

    if (this.pointerLock.isLocked && this.releaseChord && chordMatches(event, this.releaseChord)) {
      event.preventDefault();
      event.stopPropagation();
      this.pointerLock.exit();
      return;
    }

    const make = translateCodeToSet2MakeCode(event.code);
    if (!make) {
      return;
    }

    if (!event.repeat) {
      this.pressedCodes.add(event.code);
    }

    if (shouldPreventDefaultForKeyboardEvent(event)) {
      event.preventDefault();
    }

    const tsUs = toTimestampUs(event.timeStamp);
    pushSet2ScancodeSequence(this.queue, tsUs, make, true);
  };

  private readonly handleKeyUp = (event: KeyboardEvent): void => {
    if (!this.isCapturingKeyboard()) {
      return;
    }

    const make = translateCodeToSet2MakeCode(event.code);
    if (!make) {
      return;
    }

    this.pressedCodes.delete(event.code);

    if (shouldPreventDefaultForKeyboardEvent(event)) {
      event.preventDefault();
    }

    const tsUs = toTimestampUs(event.timeStamp);
    pushSet2ScancodeSequence(this.queue, tsUs, make, false);
  };

  private readonly handleMouseMove = (event: MouseEvent): void => {
    if (!this.pointerLock.isLocked) {
      return;
    }

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
    const bit = buttonToMask(event.button);
    if (bit === 0) {
      return;
    }
    event.preventDefault();
    this.setMouseButtons(this.mouseButtons | bit, event.timeStamp);
  };

  private readonly handleMouseUp = (event: MouseEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }
    const bit = buttonToMask(event.button);
    if (bit === 0) {
      return;
    }
    event.preventDefault();
    this.setMouseButtons(this.mouseButtons & ~bit, event.timeStamp);
  };

  private readonly handleWheel = (event: WheelEvent): void => {
    if (!this.isCapturingMouse()) {
      return;
    }

    // Prevent page scroll while interacting with the VM.
    event.preventDefault();

    const dz = wheelEventToSteps(event);
    if (dz === 0) {
      return;
    }

    const tsUs = toTimestampUs(event.timeStamp);
    this.queue.pushMouseWheel(tsUs, dz);
  };

  private readonly handleContextMenu = (event: Event): void => {
    if (this.isCapturingMouse()) {
      event.preventDefault();
    }
  };

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly ioWorker: InputBatchTarget,
    {
      mouseSensitivity = 1.0,
      flushHz = 125,
      releasePointerLockChord,
      logCaptureLatency = false
    }: InputCaptureOptions = {}
  ) {
    this.mouseSensitivity = mouseSensitivity;
    this.flushHz = flushHz;
    this.releaseChord = releasePointerLockChord;
    this.logCaptureLatency = logCaptureLatency;

    // Ensure the canvas can receive keyboard focus.
    if (this.canvas.tabIndex < 0) {
      this.canvas.tabIndex = 0;
    }

    this.pointerLock = new PointerLock(this.canvas);
  }

  start(): void {
    if (this.flushTimer !== null) {
      return;
    }

    this.canvas.addEventListener('click', this.handleClick);
    this.canvas.addEventListener('focus', this.handleFocus);
    this.canvas.addEventListener('blur', this.handleBlur);

    // Use capture phase to get first shot at keys before the browser scrolls.
    window.addEventListener('keydown', this.handleKeyDown, { capture: true });
    window.addEventListener('keyup', this.handleKeyUp, { capture: true });

    // Mouse events should be observed at the document level while pointer locked.
    document.addEventListener('mousemove', this.handleMouseMove, { capture: true });
    document.addEventListener('mousedown', this.handleMouseDown, { capture: true });
    document.addEventListener('mouseup', this.handleMouseUp, { capture: true });

    // Wheel must be non-passive so we can preventDefault.
    this.canvas.addEventListener('wheel', this.handleWheel, { passive: false });
    this.canvas.addEventListener('contextmenu', this.handleContextMenu);

    const intervalMs = Math.max(1, Math.round(1000 / this.flushHz));
    this.flushTimer = window.setInterval(() => this.flushNow(), intervalMs);
  }

  stop(): void {
    if (this.flushTimer === null) {
      return;
    }

    this.hasFocus = false;

    window.clearInterval(this.flushTimer);
    this.flushTimer = null;

    this.canvas.removeEventListener('click', this.handleClick);
    this.canvas.removeEventListener('focus', this.handleFocus);
    this.canvas.removeEventListener('blur', this.handleBlur);

    window.removeEventListener('keydown', this.handleKeyDown, { capture: true } as AddEventListenerOptions);
    window.removeEventListener('keyup', this.handleKeyUp, { capture: true } as AddEventListenerOptions);

    document.removeEventListener('mousemove', this.handleMouseMove, { capture: true } as AddEventListenerOptions);
    document.removeEventListener('mousedown', this.handleMouseDown, { capture: true } as AddEventListenerOptions);
    document.removeEventListener('mouseup', this.handleMouseUp, { capture: true } as AddEventListenerOptions);

    this.canvas.removeEventListener('wheel', this.handleWheel as EventListener);
    this.canvas.removeEventListener('contextmenu', this.handleContextMenu);

    this.pointerLock.exit();
    this.pointerLock.dispose();

    this.releaseAllKeys();
    this.setMouseButtons(0);
  }

  flushNow(): void {
    const latencyUs = this.queue.flush(this.ioWorker);
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
    return this.hasFocus || this.pointerLock.isLocked;
  }

  private isCapturingMouse(): boolean {
    // If pointer lock is active, we always capture. Otherwise require focus to avoid
    // eating wheel events while the user scrolls elsewhere on the page.
    return this.pointerLock.isLocked || this.hasFocus;
  }

  private releaseAllKeys(): void {
    if (this.pressedCodes.size === 0) {
      return;
    }

    const nowUs = toTimestampUs(performance.now());
    for (const code of this.pressedCodes) {
      const make = translateCodeToSet2MakeCode(code);
      if (make) {
        pushSet2ScancodeSequence(this.queue, nowUs, make, false);
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
  makeWithExtendedFlag: number,
  pressed: boolean
): void {
  const make = makeWithExtendedFlag & 0xff;
  const extended = (makeWithExtendedFlag & 0x100) !== 0;

  let packedBytes: number;
  let byteLen: number;
  if (pressed) {
    if (extended) {
      packedBytes = 0xe0 | (make << 8);
      byteLen = 2;
    } else {
      packedBytes = make;
      byteLen = 1;
    }
  } else {
    if (extended) {
      packedBytes = 0xe0 | (0xf0 << 8) | (make << 16);
      byteLen = 3;
    } else {
      packedBytes = 0xf0 | (make << 8);
      byteLen = 2;
    }
  }

  queue.pushKeyScancode(timestampUs, packedBytes, byteLen);
}

function buttonToMask(button: number): number {
  // PS/2 mouse exposes left/right/middle; ignore extra buttons for now.
  switch (button) {
    case 0:
      return 1;
    case 2:
      return 2;
    case 1:
      return 4;
    default:
      return 0;
  }
}

function wheelEventToSteps(event: WheelEvent): number {
  // Try to map varying browser delta modes into discrete wheel "clicks".
  let delta = event.deltaY;
  switch (event.deltaMode) {
    case WheelEvent.DOM_DELTA_PIXEL:
      delta /= 100;
      break;
    case WheelEvent.DOM_DELTA_LINE:
      // Leave as-is.
      break;
    case WheelEvent.DOM_DELTA_PAGE:
      delta *= 3;
      break;
  }

  const steps = delta < 0 ? Math.ceil(delta) : Math.floor(delta);
  // DOM: deltaY > 0 is scroll down; PS/2: positive is wheel up.
  return (-steps) | 0;
}
