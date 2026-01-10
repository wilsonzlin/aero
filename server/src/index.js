import { resolveConfig } from "./config.js";
import { createAeroServer } from "./server.js";

const config = resolveConfig();
const { httpServer, logger } = createAeroServer(config);

httpServer.listen(config.port, config.host, () => {
  logger.info("listening", { host: config.host, port: config.port });
});

