// Register the repo's TypeScript import resolver via Node's `register()` API.
//
// Node prints an ExperimentalWarning for the legacy `--loader` flag and may remove it in a
// future release. `--import` + `register()` is the supported replacement.
//
// This module is intended to be used with:
//   node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs ...
import { register } from "node:module";

register("./ts-strip-loader.mjs", import.meta.url);

