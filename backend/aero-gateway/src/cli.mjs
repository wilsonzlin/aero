import { startGateway } from './gateway.mjs';

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      out[key] = next;
      i += 1;
    } else {
      out[key] = true;
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const host = args.host ?? process.env.HOST ?? '127.0.0.1';
const port = Number.parseInt(args.port ?? process.env.PORT ?? '8080', 10);

const gateway = await startGateway({ host, port });

console.log(`Aero gateway listening on ${gateway.url}`);
console.log(`- TCP proxy: ws://${host}:${gateway.port}/tcp?v=1&host=127.0.0.1&port=1234`);
console.log(`- DoH:       ${gateway.url}/dns-query`);
console.log(`- Metrics:   ${gateway.url}/metrics`);

const shutdown = async (signal) => {
  console.log(`\nReceived ${signal}, shutting down...`);
  await gateway.close();
  process.exit(0);
};

process.on('SIGINT', () => void shutdown('SIGINT'));
process.on('SIGTERM', () => void shutdown('SIGTERM'));
