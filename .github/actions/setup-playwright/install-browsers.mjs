import { fail } from "../_shared/github_io.mjs";
import { execNodeCliInherit } from "../_shared/exec.mjs";

const cli = process.env.PLAYWRIGHT_CLI;
if (!cli) fail("setup-playwright: PLAYWRIGHT_CLI is not set");

const browsers = (process.env.BROWSERS || "").split(/\s+/u).filter(Boolean);
if (!browsers.length) fail("setup-playwright: no browsers requested");

const withDeps = (process.env.WITH_DEPS || "false").trim().toLowerCase() === "true";

const args = [cli, "install"];
if (withDeps) args.push("--with-deps");
args.push(...browsers);

console.log(`Running: node ${args.join(" ")}`);
execNodeCliInherit(args, { env: process.env });

