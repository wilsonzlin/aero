export function sendText(res, statusCode, message, opts = {}) {
  const body = Buffer.from(message, "utf8");
  res.statusCode = statusCode;
  if (opts.allow) res.setHeader("Allow", opts.allow);
  res.setHeader("Content-Type", "text/plain; charset=utf-8");
  res.setHeader("Content-Length", String(body.byteLength));
  res.setHeader("Cache-Control", "no-store");
  res.end(body);
}

export function sendEmpty(res, statusCode, opts = {}) {
  res.statusCode = statusCode;
  if (opts.allow) res.setHeader("Allow", opts.allow);
  res.setHeader("Cache-Control", "no-store");
  res.setHeader("Content-Length", "0");
  res.end();
}
