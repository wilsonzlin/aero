import { loadConfig } from "./config.js";
import { buildServer } from "./server.js";
import { unrefBestEffort } from "./unrefSafe.js";

async function main(): Promise<void> {
  const config = loadConfig();
  const { app, markShuttingDown, closeUpgradeSockets } = buildServer(config);

  let forceExitTimer: NodeJS.Timeout | null = null;

  async function shutdown(signal: string): Promise<void> {
    markShuttingDown();
    app.log.info({ signal }, 'Shutdown requested');

    forceExitTimer = setTimeout(() => {
      app.log.error({ graceMs: config.SHUTDOWN_GRACE_MS }, 'Graceful shutdown timed out; forcing exit');
      process.exit(1);
    }, config.SHUTDOWN_GRACE_MS);
    unrefBestEffort(forceExitTimer);

    try {
      // `fastify.close()` waits for the underlying HTTP server to close, which in turn
      // can be blocked by long-lived upgrade sockets. We explicitly destroy those to
      // ensure shutdown completes within the grace window.
      const closePromise = app.close();
      app.server.closeIdleConnections?.();
      closeUpgradeSockets();
      await closePromise;
      process.exit(0);
    } catch (err) {
      app.log.error({ err }, 'Error during shutdown');
      process.exit(1);
    }
  }

  process.once("SIGTERM", () => void shutdown("SIGTERM"));
  process.once("SIGINT", () => void shutdown("SIGINT"));

  await app.listen({ host: config.HOST, port: config.PORT });
  app.log.info(
    {
      host: config.HOST,
      port: config.PORT,
      scheme: config.TLS_ENABLED ? "https" : "http",
      trustProxy: config.TRUST_PROXY,
      allowedOrigins: config.ALLOWED_ORIGINS,
      crossOriginIsolation: config.CROSS_ORIGIN_ISOLATION,
    },
    "aero-gateway listening",
  );

  // Clean up on normal exit paths.
  process.once("exit", () => {
    if (forceExitTimer) clearTimeout(forceExitTimer);
  });
}

void main();
