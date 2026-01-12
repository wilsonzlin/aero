import { installPerfHud } from "./perf/hud_entry";
import { installAeroGlobal } from "./runtime/aero_global";

// Minimal entrypoint used by the Playwright guest CPU throughput microbench.
//
// The main app entrypoint pulls in optional demo/microbench WASM modules that may not be built
// in CI or lightweight benchmarking environments. Keeping this entrypoint small ensures that
// guest CPU benchmarks only load what is needed for the bench runner.
installPerfHud();
installAeroGlobal();

document.body.textContent = "Aero guest CPU bench ready";

