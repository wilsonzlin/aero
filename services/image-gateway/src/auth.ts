import type { FastifyRequest } from "fastify";

import type { Config } from "./config";
import { ApiError } from "./errors";

const MAX_USER_ID_LEN = 256;

export function getCallerUserId(req: FastifyRequest, config: Config): string {
  if (config.authMode === "none") return "public";

  const raw = req.headers["x-user-id"];
  const userId =
    typeof raw === "string"
      ? raw
      : Array.isArray(raw) && raw.length === 1 && typeof raw[0] === "string"
        ? raw[0]
        : undefined;

  if (!userId) {
    throw new ApiError(401, "Missing X-User-Id header (AUTH_MODE=dev)", "UNAUTH");
  }
  const trimmed = userId.trim();
  if (!trimmed) {
    throw new ApiError(400, "Invalid X-User-Id header", "BAD_REQUEST");
  }
  if (trimmed.length > MAX_USER_ID_LEN) {
    throw new ApiError(400, "X-User-Id header is too long", "BAD_REQUEST");
  }
  return trimmed;
}

