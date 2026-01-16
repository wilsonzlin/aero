const LEVELS = Object.freeze({
  debug: 10,
  info: 20,
  warn: 30,
  error: 40,
});

import { formatOneLineError } from "./text.js";

export function createLogger({ level = "info" } = {}) {
  const threshold = LEVELS[level] ?? LEVELS.info;

  function emit(levelName, msg, fields) {
    if ((LEVELS[levelName] ?? LEVELS.info) < threshold) return;
    const extra =
      fields && typeof fields === "object" && !Array.isArray(fields)
        ? fields
        : { fields };
    const entry = {
      time: new Date().toISOString(),
      level: levelName,
      msg,
      ...extra,
    };
    let line = "";
    try {
      line = JSON.stringify(entry);
    } catch (err) {
      const fallback = {
        time: entry.time,
        level: "error",
        msg: "logger: failed to stringify log entry",
        err: formatOneLineError(err, 512, "Error"),
      };
      try {
        line = JSON.stringify(fallback);
      } catch {
        return;
      }
    }
    try {
      process.stdout.write(`${line}\n`);
    } catch {
      // ignore
    }
  }

  return {
    debug: (msg, fields = {}) => emit("debug", msg, fields),
    info: (msg, fields = {}) => emit("info", msg, fields),
    warn: (msg, fields = {}) => emit("warn", msg, fields),
    error: (msg, fields = {}) => emit("error", msg, fields),
  };
}

