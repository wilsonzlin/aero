import { startL2ProxyServer } from "./server.ts";

void (async () => {
  const running = await startL2ProxyServer();
  console.log(`aero-l2-proxy listening on ${running.listenAddress}`);
})().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
