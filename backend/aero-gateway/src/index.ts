import { loadConfig } from './config.js';
import { buildServer } from './server.js';

async function main(): Promise<void> {
  const config = loadConfig();
  const { app, markShuttingDown } = buildServer(config);

  let forceExitTimer: NodeJS.Timeout | null = null;

  async function shutdown(signal: string): Promise<void> {
    markShuttingDown();
    app.log.info({ signal }, 'Shutdown requested');

    forceExitTimer = setTimeout(() => {
      app.log.error({ graceMs: config.SHUTDOWN_GRACE_MS }, 'Graceful shutdown timed out; forcing exit');
      process.exit(1);
    }, config.SHUTDOWN_GRACE_MS);
    forceExitTimer.unref();

    try {
      await app.close();
      process.exit(0);
    } catch (err) {
      app.log.error({ err }, 'Error during shutdown');
      process.exit(1);
    }
  }

  process.once('SIGTERM', () => void shutdown('SIGTERM'));
  process.once('SIGINT', () => void shutdown('SIGINT'));

  await app.listen({ host: config.HOST, port: config.PORT });
  app.log.info(
    {
      host: config.HOST,
      port: config.PORT,
      scheme: config.TLS_ENABLED ? 'https' : 'http',
      trustProxy: config.TRUST_PROXY,
      allowedOrigins: config.ALLOWED_ORIGINS,
      crossOriginIsolation: config.CROSS_ORIGIN_ISOLATION,
      trustProxy: config.TRUST_PROXY,
    },
    'aero-gateway listening',
  );

  // Clean up on normal exit paths.
  process.once('exit', () => {
    if (forceExitTimer) clearTimeout(forceExitTimer);
  });
}

void main();
