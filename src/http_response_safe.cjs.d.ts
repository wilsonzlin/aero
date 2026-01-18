import type { OutgoingHttpHeader, OutgoingHttpHeaders, ServerResponse } from "node:http";

export function tryWriteResponse(
  res: ServerResponse,
  statusCode: number,
  headers: OutgoingHttpHeaders | OutgoingHttpHeader[] | null | undefined,
  body: Buffer | string | undefined
): void;

export function sendJsonNoStore(
  res: ServerResponse,
  statusCode: number,
  value: unknown,
  opts: Readonly<{ contentType?: string }> | undefined
): void;

export function sendTextNoStore(
  res: ServerResponse,
  statusCode: number,
  body: string,
  opts: Readonly<{ contentType?: string }> | undefined
): void;

