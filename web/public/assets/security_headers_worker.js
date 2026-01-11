// Minimal module worker used by Playwright to validate:
// - `worker-src` allows same-origin module workers
// - COOP/COEP/CORP headers are set on worker script responses
// - CSP does not inadvertently block worker startup
self.postMessage("ok");
