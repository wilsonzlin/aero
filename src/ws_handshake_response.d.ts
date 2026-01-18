import type { Duplex } from "node:stream";

export type WebSocketHandshakeResponseOptions = Readonly<{
  key: string;
  protocol?: string;
}>;

export function computeWebSocketAccept(key: string): string;

export function encodeWebSocketHandshakeResponse(opts: WebSocketHandshakeResponseOptions): string;

export function writeWebSocketHandshake(socket: Duplex, opts: WebSocketHandshakeResponseOptions): void;

