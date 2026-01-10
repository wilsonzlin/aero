import { startProxyServer } from "./server";

void startProxyServer().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err);
  process.exitCode = 1;
});

