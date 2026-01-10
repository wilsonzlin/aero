export interface PointerLockCallbacks {
  onChange?: (locked: boolean) => void;
  onError?: (event: Event) => void;
}

export class PointerLock {
  private readonly onChange?: (locked: boolean) => void;
  private readonly onError?: (event: Event) => void;

  private locked = false;

  private readonly handleChange = (): void => {
    const next = document.pointerLockElement === this.element;
    if (next === this.locked) {
      return;
    }
    this.locked = next;
    this.onChange?.(next);
  };

  private readonly handleError = (event: Event): void => {
    this.onError?.(event);
  };

  constructor(
    private readonly element: HTMLElement,
    { onChange, onError }: PointerLockCallbacks = {}
  ) {
    this.onChange = onChange;
    this.onError = onError;
    this.locked = document.pointerLockElement === this.element;

    document.addEventListener('pointerlockchange', this.handleChange);
    document.addEventListener('pointerlockerror', this.handleError);
  }

  get isLocked(): boolean {
    return this.locked;
  }

  get isSupported(): boolean {
    return typeof this.element.requestPointerLock === 'function';
  }

  request(): void {
    if (!this.isSupported) {
      return;
    }
    if (document.pointerLockElement === this.element) {
      return;
    }
    try {
      this.element.requestPointerLock();
    } catch {
      // Some browsers throw synchronously if not allowed (no user gesture).
    }
  }

  exit(): void {
    if (document.pointerLockElement !== this.element) {
      return;
    }
    try {
      document.exitPointerLock();
    } catch {
      // Ignore.
    }
  }

  dispose(): void {
    document.removeEventListener('pointerlockchange', this.handleChange);
    document.removeEventListener('pointerlockerror', this.handleError);
  }
}

