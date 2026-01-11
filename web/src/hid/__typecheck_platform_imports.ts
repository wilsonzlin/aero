// This file is intentionally unused at runtime.
//
// The root CI gate only runs `npm run typecheck` (root `tsconfig.json`) and does
// not execute the `web` workspace typecheck (which has a heavy WASM build
// pre-step). `web/src/platform/*` isn't included in the root TS program by
// default, so WebHID platform code can accidentally accumulate type errors.
//
// Import the WebHID passthrough implementation so that it is typechecked by the
// existing CI `npm run typecheck` job without affecting the web app bundle.
import "../platform/webhid_passthrough.ts";

