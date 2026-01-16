import path from "node:path";
import { createRequire } from "node:module";
import fs from "node:fs";
import { fail, requireEnv } from "../_shared/github_io.mjs";

const outPath = requireEnv("GITHUB_OUTPUT");

const require = createRequire(import.meta.url);
const pkgJsonPath = require.resolve("playwright-core/package.json", { paths: [process.cwd()] });
const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
const rootDir = path.dirname(pkgJsonPath);
const cli = path.join(rootDir, "cli.js");

if (!fs.existsSync(cli)) {
  fail(`setup-playwright: expected Playwright CLI at ${cli} but it does not exist`);
}

fs.appendFileSync(outPath, `version=${pkg.version}\ncli=${cli}\n`, "utf8");

