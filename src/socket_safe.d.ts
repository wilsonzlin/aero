export function callMethodCaptureErrorBestEffort(obj: unknown, key: PropertyKey, ...args: unknown[]): unknown | null;
export function writeCaptureErrorBestEffort(
  stream: unknown,
  ...args: unknown[]
): Readonly<{ ok: boolean; err: unknown | null }>;
export function tryGetMethodBestEffort(obj: unknown, key: PropertyKey): ((this: unknown, ...args: unknown[]) => unknown) | null;
export function callMethodBestEffort(obj: unknown, key: PropertyKey, ...args: unknown[]): boolean;

export function destroyBestEffort(obj: unknown): void;
export function destroyWithErrorBestEffort(obj: unknown, err: unknown): void;
export function closeBestEffort(obj: unknown, ...args: unknown[]): void;

export function pauseBestEffort(stream: unknown): void;
export function resumeBestEffort(stream: unknown): void;
export function pauseRequired(stream: unknown): boolean;
export function resumeRequired(stream: unknown): boolean;

export function endBestEffort(stream: unknown, ...args: unknown[]): void;
export function endRequired(stream: unknown, ...args: unknown[]): boolean;
export function endCaptureErrorBestEffort(stream: unknown, ...args: unknown[]): unknown | null;

export function removeAllListenersBestEffort(emitter: unknown): void;

export function setNoDelayBestEffort(socket: unknown, noDelay: boolean): void;
export function setNoDelayRequired(socket: unknown, noDelay: boolean): boolean;
export function setTimeoutRequired(socket: unknown, timeoutMs: number): boolean;

