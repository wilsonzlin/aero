const http = require('http');
const fs = require('fs');
const path = require('path');

function parseArgs(argv) {
  const out = { host: '127.0.0.1', port: 4173 };
  for (let i = 2; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--port') out.port = Number(argv[++i]);
    else if (arg === '--host') out.host = argv[++i];
  }
  return out;
}

const { host, port } = parseArgs(process.argv);
const rootDir = path.join(__dirname, '..');

function contentTypeFor(filePath) {
  switch (path.extname(filePath)) {
    case '.html':
      return 'text/html; charset=utf-8';
    case '.js':
      return 'text/javascript; charset=utf-8';
    case '.css':
      return 'text/css; charset=utf-8';
    case '.json':
      return 'application/json; charset=utf-8';
    case '.wasm':
      return 'application/wasm';
    default:
      return 'application/octet-stream';
  }
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url ?? '/', `http://${req.headers.host ?? host}`);
  const rawPath = url.pathname === '/' ? '/index.html' : url.pathname;

  const resolvedPath = path.normalize(path.join(rootDir, rawPath));
  if (!resolvedPath.startsWith(rootDir)) {
    res.writeHead(403);
    res.end('Forbidden');
    return;
  }

  fs.readFile(resolvedPath, (err, data) => {
    if (err) {
      res.writeHead(404);
      res.end('Not found');
      return;
    }

    res.writeHead(200, {
      'Content-Type': contentTypeFor(resolvedPath),
      // The real Aero project requires COOP/COEP for SharedArrayBuffer. Keeping these
      // headers here makes the demo behave like production from day one.
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    });
    res.end(data);
  });
});

server.listen(port, host, () => {
  console.log(`Serving ${rootDir} at http://${host}:${port}/`);
});

process.on('SIGTERM', () => server.close());
process.on('SIGINT', () => server.close());

