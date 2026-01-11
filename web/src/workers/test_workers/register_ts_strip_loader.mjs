import { register } from "node:module";
import { pathToFileURL } from "node:url";

// Enable the repo's TypeScript-friendly ESM resolver (extensionless + `.js`->`.ts`
// fallback) for worker_threads tests. This avoids having to rewrite production
// worker import specifiers just to satisfy Node's ESM resolution rules.
const loaderUrl = new URL("../../../../scripts/ts-strip-loader.mjs", import.meta.url);
register(loaderUrl.href, pathToFileURL("./"));

