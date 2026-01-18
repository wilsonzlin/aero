import type { Buffer } from "node:buffer";

export type WsSendQueue = Readonly<{
  enqueue(frame: Buffer): void;
  isBackpressured(): boolean;
  backlogBytes(): number;
  close(): void;
}>;

export type WsSendQueueOptions = Readonly<{
  ws?: unknown;
  highWatermarkBytes?: number;
  lowWatermarkBytes?: number;
  pollMs?: number;
  onPauseSources?: () => void;
  onResumeSources?: () => void;
  onSendError?: (err: unknown) => void;
}>;

export function createWsSendQueue(opts?: WsSendQueueOptions): WsSendQueue;

