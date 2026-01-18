import type { Duplex } from "node:stream";

export function rejectHttpUpgrade(socket: Duplex, statusCode: number, message: unknown): void;
