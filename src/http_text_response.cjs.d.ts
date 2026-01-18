import type { Buffer } from "node:buffer";

export type EncodeHttpTextResponseOptions = Readonly<{
  statusCode: number;
  statusText: string;
  bodyText?: string;
  contentType?: string;
  cacheControl?: string | null;
  closeConnection?: boolean;
}>;

export function encodeHttpTextResponse(opts: EncodeHttpTextResponseOptions): Buffer;
