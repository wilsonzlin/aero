export type PointerLockInputEvent =
  | { type: "pointerLockChange"; locked: boolean }
  | { type: "mouseMove"; dx: number; dy: number }
  | { type: "mouseButton"; button: number; down: boolean }
  | { type: "mouseWheel"; deltaX: number; deltaY: number; deltaZ: number };

export type KeyboardInputEvent =
  | { type: "keyDown"; code: string; repeat: boolean; altKey: boolean; ctrlKey: boolean; shiftKey: boolean; metaKey: boolean }
  | { type: "keyUp"; code: string; altKey: boolean; ctrlKey: boolean; shiftKey: boolean; metaKey: boolean };

export type InputEvent = PointerLockInputEvent | KeyboardInputEvent;

export type InputHandlers = {
  onEvent(event: InputEvent): void;
  onError?(error: Error): void;
};

export type DetachFn = () => void;

export function attachPointerLock(canvas: HTMLCanvasElement, handlers: InputHandlers): DetachFn {
  if (typeof document === "undefined") {
    const err = new Error("Pointer lock must be attached from a Window context (document is missing).");
    handlers.onError?.(err);
    return () => {};
  }

  if (typeof canvas.requestPointerLock !== "function") {
    const err = new Error("Pointer lock is not supported in this browser (requestPointerLock missing).");
    handlers.onError?.(err);
    return () => {};
  }

  const onClick = (ev: MouseEvent) => {
    if (document.pointerLockElement !== canvas) {
      // Avoid bubbling the click to other UI handlers (e.g. toggles) while we're
      // trying to transition into capture mode.
      ev.preventDefault();
      ev.stopPropagation();
      canvas.requestPointerLock();
      canvas.focus();
    }
  };

  const onMouseMove = (ev: MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({ type: "mouseMove", dx: ev.movementX, dy: ev.movementY });
  };

  const onMouseDown = (ev: MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({ type: "mouseButton", button: ev.button, down: true });
  };

  const onMouseUp = (ev: MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({ type: "mouseButton", button: ev.button, down: false });
  };

  const onWheel = (ev: WheelEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({ type: "mouseWheel", deltaX: ev.deltaX, deltaY: ev.deltaY, deltaZ: ev.deltaZ });
  };

  const syncLockedState = () => {
    const locked = document.pointerLockElement === canvas;
    handlers.onEvent({ type: "pointerLockChange", locked });

    if (locked) {
      document.addEventListener("mousemove", onMouseMove);
      document.addEventListener("mousedown", onMouseDown);
      document.addEventListener("mouseup", onMouseUp);
      // Wheel must be non-passive so we can preventDefault and avoid scrolling.
      document.addEventListener("wheel", onWheel, { passive: false });
    } else {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("mouseup", onMouseUp);
      document.removeEventListener("wheel", onWheel);
    }
  };

  canvas.addEventListener("click", onClick);
  document.addEventListener("pointerlockchange", syncLockedState);
  document.addEventListener("pointerlockerror", syncLockedState);

  return () => {
    canvas.removeEventListener("click", onClick);
    document.removeEventListener("pointerlockchange", syncLockedState);
    document.removeEventListener("pointerlockerror", syncLockedState);
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mousedown", onMouseDown);
    document.removeEventListener("mouseup", onMouseUp);
    document.removeEventListener("wheel", onWheel);
  };
}

export function attachKeyboard(canvas: HTMLCanvasElement, handlers: InputHandlers): DetachFn {
  // Allow the canvas to receive focus.
  if (canvas.tabIndex < 0) {
    canvas.tabIndex = 0;
  }

  const onKeyDown = (ev: KeyboardEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({
      type: "keyDown",
      code: ev.code,
      repeat: ev.repeat,
      altKey: ev.altKey,
      ctrlKey: ev.ctrlKey,
      shiftKey: ev.shiftKey,
      metaKey: ev.metaKey,
    });
  };

  const onKeyUp = (ev: KeyboardEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    handlers.onEvent({
      type: "keyUp",
      code: ev.code,
      altKey: ev.altKey,
      ctrlKey: ev.ctrlKey,
      shiftKey: ev.shiftKey,
      metaKey: ev.metaKey,
    });
  };

  canvas.addEventListener("keydown", onKeyDown);
  canvas.addEventListener("keyup", onKeyUp);

  return () => {
    canvas.removeEventListener("keydown", onKeyDown);
    canvas.removeEventListener("keyup", onKeyUp);
  };
}
