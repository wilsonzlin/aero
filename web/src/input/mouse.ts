export type MousePacketSink = (dx: number, dy: number, wheel: number) => void;
export type MouseButtonSink = (button: number, pressed: boolean) => void;

export class MouseCapture {
  private pointerLocked = false;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly onMove: MousePacketSink,
    private readonly onButton: MouseButtonSink,
  ) {}

  attach(): void {
    this.canvas.addEventListener("click", () => {
      void this.canvas.requestPointerLock();
      this.canvas.focus();
    });

    document.addEventListener("pointerlockchange", () => {
      this.pointerLocked = document.pointerLockElement === this.canvas;
    });

    this.canvas.addEventListener("mousemove", (e) => {
      if (!this.pointerLocked) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      this.onMove(e.movementX, e.movementY, 0);
    });

    this.canvas.addEventListener("mousedown", (e) => {
      if (!this.pointerLocked) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      this.onButton(e.button, true);
    });

    this.canvas.addEventListener("mouseup", (e) => {
      if (!this.pointerLocked) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      this.onButton(e.button, false);
    });

    this.canvas.addEventListener(
      "wheel",
      (e) => {
        if (!this.pointerLocked) {
          return;
        }
        e.preventDefault();
        e.stopPropagation();
        // Wheel deltas are browser-defined; the host glue should convert this to
        // the guest's expected units.
        this.onMove(0, 0, e.deltaY);
      },
      { passive: false },
    );
  }
}
