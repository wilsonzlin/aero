import { execFileSync } from "node:child_process";
import { fail } from "../_shared/github_io.mjs";

const cli = process.env.PLAYWRIGHT_CLI;
if (!cli) fail("setup-playwright: PLAYWRIGHT_CLI is not set");

const browsers = (process.env.BROWSERS || "").split(/\s+/u).filter(Boolean);
if (!browsers.length) fail("setup-playwright: no browsers requested");

const withDeps = (process.env.WITH_DEPS || "false").trim().toLowerCase() === "true";

const args = [cli, "install"];
if (withDeps) args.push("--with-deps");
args.push(...browsers);

console.log(`Running: node ${args.join(" ")}`);
execFileSync(process.execPath, args, { env: process.env, stdio: "inherit" });

