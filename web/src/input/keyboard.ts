import { ps2Set2BytesForKeyEvent } from "./scancodes";

export type KeyboardScancodeSink = (bytes: number[]) => void;

export class KeyboardCapture {
  private pressed = new Set<string>();

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly sink: KeyboardScancodeSink,
  ) {}

  attach(): void {
    // Ensure the canvas can receive focus.
    if (!this.canvas.hasAttribute("tabindex")) {
      this.canvas.tabIndex = 0;
    }

    this.canvas.addEventListener("keydown", (e) => this.onKeyDown(e));
    this.canvas.addEventListener("keyup", (e) => this.onKeyUp(e));
    this.canvas.addEventListener("blur", () => this.releaseAll());
  }

  private onKeyDown(e: KeyboardEvent): void {
    const bytes = ps2Set2BytesForKeyEvent(e.code, true);
    if (!bytes) {
      return;
    }
    e.preventDefault();
    e.stopPropagation();

    // Browser auto-repeat matches the keyboard's typematic behaviour: repeated
    // make codes while the key stays pressed.
    if (!e.repeat) {
      this.pressed.add(e.code);
    }
    this.sink(bytes);
  }

  private onKeyUp(e: KeyboardEvent): void {
    const bytes = ps2Set2BytesForKeyEvent(e.code, false);
    if (!bytes) {
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    this.pressed.delete(e.code);
    if (bytes.length > 0) {
      this.sink(bytes);
    }
  }

  private releaseAll(): void {
    for (const code of this.pressed) {
      const bytes = ps2Set2BytesForKeyEvent(code, false);
      if (!bytes) {
        continue;
      }
      if (bytes.length > 0) {
        this.sink(bytes);
      }
    }
    this.pressed.clear();
  }
}
