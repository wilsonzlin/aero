import type { ServerResponse } from "node:http";

export function sendText(
  res: ServerResponse,
  statusCode: number,
  message: string,
  opts?: { allow?: string },
): void;

export function sendEmpty(res: ServerResponse, statusCode: number, opts?: { allow?: string }): void;
