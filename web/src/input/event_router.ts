import type { HostInputEvent, InputEventKind } from "./types";
import type { InputQueue, InputQueueSnapshot } from "./queue";

export interface InputEventRouterHooks {
  on_capture?: (args: { id: number; kind: InputEventKind; t_capture_ms: number }) => void;
  on_injected?: (args: {
    id: number;
    kind: InputEventKind;
    t_injected_ms: number;
    enqueued: boolean;
    queue: InputQueueSnapshot;
  }) => void;
}

export class InputEventRouter {
  private readonly target: EventTarget;
  private readonly queue: InputQueue<HostInputEvent>;
  private readonly hooks: InputEventRouterHooks;
  private nextId = 1;

  private boundKeyDown?: (e: KeyboardEvent) => void;
  private boundKeyUp?: (e: KeyboardEvent) => void;
  private boundPointerDown?: (e: PointerEvent) => void;
  private boundPointerUp?: (e: PointerEvent) => void;
  private boundPointerMove?: (e: PointerEvent) => void;
  private boundWheel?: (e: WheelEvent) => void;
  private boundContextMenu?: (e: Event) => void;

  constructor(args: { target: EventTarget; queue: InputQueue<HostInputEvent>; hooks?: InputEventRouterHooks }) {
    this.target = args.target;
    this.queue = args.queue;
    this.hooks = args.hooks ?? {};
  }

  start() {
    if (this.boundKeyDown) return;

    this.boundKeyDown = (e) => this.handleKeyboard(e, "keydown");
    this.boundKeyUp = (e) => this.handleKeyboard(e, "keyup");
    this.boundPointerDown = (e) => this.handlePointer(e, "pointerdown");
    this.boundPointerUp = (e) => this.handlePointer(e, "pointerup");
    this.boundPointerMove = (e) => this.handlePointer(e, "pointermove");
    this.boundWheel = (e) => this.handleWheel(e);
    this.boundContextMenu = (e) => {
      e.preventDefault();
      e.stopPropagation();
    };

    this.target.addEventListener("keydown", this.boundKeyDown as EventListener);
    this.target.addEventListener("keyup", this.boundKeyUp as EventListener);

    this.target.addEventListener("pointerdown", this.boundPointerDown as EventListener);
    this.target.addEventListener("pointerup", this.boundPointerUp as EventListener);
    this.target.addEventListener("pointermove", this.boundPointerMove as EventListener);
    this.target.addEventListener("wheel", this.boundWheel as EventListener, { passive: false } as AddEventListenerOptions);

    this.target.addEventListener("contextmenu", this.boundContextMenu as EventListener);
  }

  stop() {
    if (!this.boundKeyDown) return;
    this.target.removeEventListener("keydown", this.boundKeyDown as EventListener);
    this.target.removeEventListener("keyup", this.boundKeyUp as EventListener);
    this.target.removeEventListener("pointerdown", this.boundPointerDown as EventListener);
    this.target.removeEventListener("pointerup", this.boundPointerUp as EventListener);
    this.target.removeEventListener("pointermove", this.boundPointerMove as EventListener);
    this.target.removeEventListener("wheel", this.boundWheel as EventListener);
    this.target.removeEventListener("contextmenu", this.boundContextMenu as EventListener);

    this.boundKeyDown = undefined;
    this.boundKeyUp = undefined;
    this.boundPointerDown = undefined;
    this.boundPointerUp = undefined;
    this.boundPointerMove = undefined;
    this.boundWheel = undefined;
    this.boundContextMenu = undefined;
  }

  private handleKeyboard(e: KeyboardEvent, kind: InputEventKind) {
    e.preventDefault();
    e.stopPropagation();

    const id = this.nextId++;
    const t_capture_ms = performance.now();
    this.hooks.on_capture?.({ id, kind, t_capture_ms });

    const event: HostInputEvent = {
      id,
      kind,
      t_capture_ms,
      payload: {
        code: e.code,
        key: e.key,
        location: e.location,
        repeat: e.repeat,
        ctrl: e.ctrlKey,
        alt: e.altKey,
        shift: e.shiftKey,
        meta: e.metaKey,
      },
    };

    const enqueued = this.queue.push(event);
    const t_injected_ms = performance.now();
    this.hooks.on_injected?.({ id, kind, t_injected_ms, enqueued, queue: this.queue.snapshot() });
  }

  private handlePointer(e: PointerEvent, kind: InputEventKind) {
    e.preventDefault();
    e.stopPropagation();

    const id = this.nextId++;
    const t_capture_ms = performance.now();
    this.hooks.on_capture?.({ id, kind, t_capture_ms });

    const event: HostInputEvent = {
      id,
      kind,
      t_capture_ms,
      payload: {
        pointerId: e.pointerId,
        pointerType: e.pointerType,
        button: e.button,
        buttons: e.buttons,
        clientX: e.clientX,
        clientY: e.clientY,
        movementX: (e as any).movementX ?? 0,
        movementY: (e as any).movementY ?? 0,
        pressure: e.pressure,
        tiltX: e.tiltX,
        tiltY: e.tiltY,
      },
    };

    const enqueued = this.queue.push(event);
    const t_injected_ms = performance.now();
    this.hooks.on_injected?.({ id, kind, t_injected_ms, enqueued, queue: this.queue.snapshot() });
  }

  private handleWheel(e: WheelEvent) {
    e.preventDefault();
    e.stopPropagation();

    const id = this.nextId++;
    const kind: InputEventKind = "wheel";
    const t_capture_ms = performance.now();
    this.hooks.on_capture?.({ id, kind, t_capture_ms });

    const event: HostInputEvent = {
      id,
      kind,
      t_capture_ms,
      payload: {
        deltaX: e.deltaX,
        deltaY: e.deltaY,
        deltaZ: e.deltaZ,
        deltaMode: e.deltaMode,
      },
    };

    const enqueued = this.queue.push(event);
    const t_injected_ms = performance.now();
    this.hooks.on_injected?.({ id, kind, t_injected_ms, enqueued, queue: this.queue.snapshot() });
  }
}
