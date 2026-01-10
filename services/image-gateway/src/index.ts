import { buildApp } from "./app";
import { loadConfig } from "./config";
import { createS3Client } from "./s3";
import { MemoryImageStore } from "./store";

async function main(): Promise<void> {
  const config = loadConfig();
  const s3 = createS3Client(config);
  const store = new MemoryImageStore();

  const app = buildApp({ config, s3, store });

  await app.listen({ port: config.port, host: "0.0.0.0" });
}

main().catch((err) => {
  // eslint-disable-next-line no-console
  console.error(err);
  process.exitCode = 1;
});

