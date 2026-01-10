import { installPerfHud } from "./perf/hud_entry";
import { installAeroGlobal } from "./runtime/aero_global";

// Minimal entrypoint used by the Playwright storage I/O microbench.
//
// The main app entrypoint pulls in optional demo/microbench WASM modules that may not be built
// in CI or lightweight benchmarking environments. Keeping this entrypoint small ensures that
// storage benchmarks remain emulator-independent and can run against a clean checkout.
installPerfHud();
installAeroGlobal();

document.body.textContent = "Aero storage bench ready";

