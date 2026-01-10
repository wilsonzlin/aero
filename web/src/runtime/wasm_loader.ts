export type WasmVariant = "threaded" | "single";

export interface WasmApi {
    greet(name: string): string;
    add(a: number, b: number): number;
    AeroApi: new () => { version(): string; free(): void };
    // Optional audio exports (present when the WASM build includes the audio worklet bridge).
    WorkletBridge?: new (capacityFrames: number, channelCount: number) => {
        readonly shared_buffer: SharedArrayBuffer;
        readonly capacity_frames: number;
        readonly channel_count: number;
        write_f32_interleaved(samples: Float32Array): number;
        buffer_level_frames(): number;
        underrun_count(): number;
        free(): void;
    };
    create_worklet_bridge?: (capacityFrames: number, channelCount: number) => unknown;
    attach_worklet_bridge?: (sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => unknown;
    SineTone?: new () => {
        write(
            bridge: unknown,
            frames: number,
            freqHz: number,
            sampleRate: number,
            gain: number,
        ): number;
        free(): void;
    };
}

export interface WasmInitResult {
    api: WasmApi;
    variant: WasmVariant;
    reason: string;
}

export interface WasmInitOptions {
    /**
     * - `auto` (default): pick the best variant for this runtime.
     * - `threaded`: require the shared-memory build and throw if unavailable.
     * - `single`: force the non-shared-memory build.
     */
    variant?: WasmVariant | "auto";
}

interface ThreadSupport {
    supported: boolean;
    reason: string;
}

function detectThreadSupport(): ThreadSupport {
    if (typeof globalThis === "undefined") {
        return { supported: false, reason: "Not running in a JS environment with globalThis" };
    }

    // `crossOriginIsolated` is required for SharedArrayBuffer on the web.
    if (!(globalThis as any).crossOriginIsolated) {
        return {
            supported: false,
            reason: "crossOriginIsolated is false (missing COOP/COEP headers); SharedArrayBuffer is unavailable",
        };
    }

    if (typeof SharedArrayBuffer === "undefined") {
        return { supported: false, reason: "SharedArrayBuffer is undefined (not supported or not enabled)" };
    }

    if (typeof Atomics === "undefined") {
        return { supported: false, reason: "Atomics is undefined (WASM threads are not supported)" };
    }

    if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory === "undefined") {
        return { supported: false, reason: "WebAssembly.Memory is unavailable in this environment" };
    }

    try {
        // Even with SAB present, some environments may not support shared WebAssembly.Memory.
        // This is the most direct capability probe.
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return { supported: false, reason: `Shared WebAssembly.Memory is not supported: ${message}` };
    }

    return { supported: true, reason: "crossOriginIsolated + SharedArrayBuffer + Atomics + shared WebAssembly.Memory" };
}

type RawWasmModule = any;

function toApi(mod: RawWasmModule): WasmApi {
    return {
        greet: mod.greet,
        add: mod.add,
        AeroApi: mod.AeroApi,
        WorkletBridge: mod.WorkletBridge,
        create_worklet_bridge: mod.create_worklet_bridge,
        attach_worklet_bridge: mod.attach_worklet_bridge,
        SineTone: mod.SineTone,
    };
}

// `wasm-pack` outputs into `web/src/wasm/pkg-single` and `web/src/wasm/pkg-threaded`.
//
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout. Use `import.meta.glob` so:
//  - Vite builds don't fail when the generated output is missing.
//  - When the output *is* present, it is bundled as usual.
const wasmImporters = import.meta.glob("../wasm/pkg-*/aero_wasm.js");

async function loadSingle(): Promise<WasmApi> {
    const importer = wasmImporters["../wasm/pkg-single/aero_wasm.js"];
    if (!importer) {
        throw new Error(
            "Missing single-thread WASM package. Build it with: node ./scripts/build_wasm.mjs single",
        );
    }
    const mod = (await importer()) as RawWasmModule;
    await mod.default();
    return toApi(mod);
}

async function loadThreaded(): Promise<WasmApi> {
    const importer = wasmImporters["../wasm/pkg-threaded/aero_wasm.js"];
    if (!importer) {
        throw new Error(
            "Missing threaded WASM package. Build it with: node ./scripts/build_wasm.mjs threaded",
        );
    }
    const mod = (await importer()) as RawWasmModule;
    await mod.default();
    return toApi(mod);
}

export async function initWasm(options: WasmInitOptions = {}): Promise<WasmInitResult> {
    const requested = options.variant ?? "auto";
    const threadSupport = detectThreadSupport();

    if (requested === "threaded") {
        if (!threadSupport.supported) {
            throw new Error(
                [
                    "Threaded WASM build requested but the current runtime cannot use shared-memory WebAssembly.",
                    `Reason: ${threadSupport.reason}`,
                    "",
                    "To enable the threaded build in browsers you must serve the page with COOP/COEP headers so that",
                    "`crossOriginIsolated === true`, e.g.:",
                    "  Cross-Origin-Opener-Policy: same-origin",
                    "  Cross-Origin-Embedder-Policy: require-corp",
                ].join("\n"),
            );
        }

        return {
            api: await loadThreaded(),
            variant: "threaded",
            reason: threadSupport.reason,
        };
    }

    if (requested === "single") {
        return { api: await loadSingle(), variant: "single", reason: "Forced via initWasm({ variant: 'single' })" };
    }

    if (threadSupport.supported) {
        return { api: await loadThreaded(), variant: "threaded", reason: threadSupport.reason };
    }

    return { api: await loadSingle(), variant: "single", reason: threadSupport.reason };
}
