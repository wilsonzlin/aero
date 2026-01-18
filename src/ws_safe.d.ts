export type WsSafeSendData = string | ArrayBuffer | ArrayBufferView;
export type WsSafeSendCallback = (err?: unknown) => void;

export function wsIsOpenSafe(ws: unknown): boolean;

export function wsSendSafe(ws: unknown, data: WsSafeSendData, cb?: WsSafeSendCallback): boolean;

export function wsCloseSafe(ws: unknown, code?: number, reason?: unknown): void;
