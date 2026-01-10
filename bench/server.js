'use strict';

const http = require('node:http');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const path = require('node:path');

const CONTENT_TYPES = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.json', 'application/json; charset=utf-8'],
  ['.wasm', 'application/wasm'],
  ['.png', 'image/png'],
  ['.jpg', 'image/jpeg'],
  ['.jpeg', 'image/jpeg'],
  ['.svg', 'image/svg+xml; charset=utf-8']
]);

function contentType(filePath) {
  return CONTENT_TYPES.get(path.extname(filePath).toLowerCase()) ?? 'application/octet-stream';
}

function safeResolve(rootDir, requestPath) {
  const resolved = path.resolve(rootDir, '.' + requestPath);
  const rootResolved = path.resolve(rootDir);
  if (!resolved.startsWith(rootResolved + path.sep) && resolved !== rootResolved) {
    throw new Error('Path traversal attempt rejected');
  }
  return resolved;
}

/**
 * @param {{ rootDir: string, host?: string, port?: number }} opts
 * @returns {Promise<{ baseUrl: string, close: () => Promise<void> }>}
 */
async function startStaticServer(opts) {
  const host = opts.host ?? '127.0.0.1';
  const port = opts.port ?? 0;
  const rootDir = opts.rootDir;

  const server = http.createServer(async (req, res) => {
    try {
      const requestUrl = new URL(req.url ?? '/', `http://${host}`);
      let pathname = decodeURIComponent(requestUrl.pathname);
      if (pathname === '/') pathname = '/index.html';

      const filePath = safeResolve(rootDir, pathname);
      const st = await fsp.stat(filePath).catch(() => null);
      if (!st || !st.isFile()) {
        res.statusCode = 404;
        res.setHeader('Content-Type', 'text/plain; charset=utf-8');
        res.end('Not found');
        return;
      }

      res.statusCode = 200;
      res.setHeader('Content-Type', contentType(filePath));
      fs.createReadStream(filePath).pipe(res);
    } catch (err) {
      res.statusCode = 500;
      res.setHeader('Content-Type', 'text/plain; charset=utf-8');
      res.end(String(err?.stack || err));
    }
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => resolve());
  });

  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Unexpected server address');

  return {
    baseUrl: `http://${host}:${address.port}/`,
    close: () =>
      new Promise((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      })
  };
}

module.exports = { startStaticServer };
