const LEVELS = Object.freeze({
  debug: 10,
  info: 20,
  warn: 30,
  error: 40,
});

export function createLogger({ level = "info" } = {}) {
  const threshold = LEVELS[level] ?? LEVELS.info;

  function emit(levelName, msg, fields) {
    if ((LEVELS[levelName] ?? LEVELS.info) < threshold) return;
    const entry = {
      time: new Date().toISOString(),
      level: levelName,
      msg,
      ...fields,
    };
    process.stdout.write(`${JSON.stringify(entry)}\n`);
  }

  return {
    debug: (msg, fields = {}) => emit("debug", msg, fields),
    info: (msg, fields = {}) => emit("info", msg, fields),
    warn: (msg, fields = {}) => emit("warn", msg, fields),
    error: (msg, fields = {}) => emit("error", msg, fields),
  };
}

