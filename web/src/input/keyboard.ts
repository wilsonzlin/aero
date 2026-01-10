import { codeToSet2, set2Break, set2Make } from "./scancode";

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
    const sc = codeToSet2(e.code);
    if (!sc) {
      return;
    }
    e.preventDefault();

    // Browser auto-repeat matches the keyboard's typematic behaviour: repeated
    // make codes while the key stays pressed.
    if (!e.repeat) {
      this.pressed.add(e.code);
    }
    this.sink(set2Make(sc));
  }

  private onKeyUp(e: KeyboardEvent): void {
    const sc = codeToSet2(e.code);
    if (!sc) {
      return;
    }
    e.preventDefault();
    this.pressed.delete(e.code);
    this.sink(set2Break(sc));
  }

  private releaseAll(): void {
    for (const code of this.pressed) {
      const sc = codeToSet2(code);
      if (!sc) {
        continue;
      }
      this.sink(set2Break(sc));
    }
    this.pressed.clear();
  }
}

