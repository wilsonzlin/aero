import type { FastifyRequest } from "fastify";

import type { Config } from "./config";
import { ApiError } from "./errors";

export function getCallerUserId(req: FastifyRequest, config: Config): string {
  if (config.authMode === "none") return "public";

  const raw = req.headers["x-user-id"];
  const userId =
    typeof raw === "string" ? raw : Array.isArray(raw) ? raw[0] : undefined;

  if (!userId) {
    throw new ApiError(401, "Missing X-User-Id header (AUTH_MODE=dev)", "UNAUTH");
  }
  return userId;
}

