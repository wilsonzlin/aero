export type WasmVariant = "threaded" | "single";

export type MicBridgeHandle = {
    buffered_samples(): number;
    dropped_samples(): number;
    read_f32_into(out: Float32Array): number;
    free(): void;
};

export interface WasmApi {
    greet(name: string): string;
    add(a: number, b: number): number;
    version: () => number;
    sum: (a: number, b: number) => number;
    mem_store_u32: (offset: number, value: number) => void;
    mem_load_u32: (offset: number) => number;
    /**
     * Demo renderer: writes an RGBA8888 test pattern into WASM linear memory at `dstOffset`.
     *
     * Optional for dev builds where the generated wasm-bindgen package hasn't been rebuilt yet.
     */
    demo_render_rgba8888?: (
        dstOffset: number,
        width: number,
        height: number,
        strideBytes: number,
        nowMs: number,
    ) => number;
    /**
     * Guest RAM layout contract (see `docs/adr/0003-shared-memory-layout.md`).
     *
     * Returns a small object describing where guest physical address 0 maps
     * inside the wasm linear memory.
     */
    guest_ram_layout: (desiredBytes: number) => {
        readonly guest_base: number;
        readonly guest_size: number;
        readonly runtime_reserved: number;
    };

    UsbHidBridge: new () => {
        keyboard_event(usage: number, pressed: boolean): void;
        mouse_move(dx: number, dy: number): void;
        mouse_buttons(buttons: number): void;
        mouse_wheel(delta: number): void;
        gamepad_report(packedLo: number, packedHi: number): void;
        drain_next_keyboard_report(): Uint8Array | null;
        drain_next_mouse_report(): Uint8Array | null;
        drain_next_gamepad_report(): Uint8Array | null;
        free(): void;
    };

    CpuWorkerDemo?: new (
        ramSizeBytes: number,
        framebufferOffsetBytes: number,
        width: number,
        height: number,
        tileSize: number,
        guestCounterOffsetBytes: number,
    ) => {
        tick(nowMs: number): number;
        render_frame(frameSeq: number, nowMs: number): number;
        free(): void;
    };
    AeroApi: new () => { version(): string; free(): void };
    DemoVm: new (ramSizeBytes: number) => {
        run_steps(steps: number): void;
        serial_output(): Uint8Array;
        /**
         * Returns the number of bytes currently in the demo VM's serial output buffer.
         *
         * Optional for older WASM builds; prefer this over calling `serial_output()`
         * when you only need the length (to avoid copying large buffers into JS).
         */
        serial_output_len?(): number;
        snapshot_full(): Uint8Array;
        snapshot_dirty(): Uint8Array;
        restore_snapshot(bytes: Uint8Array): void;
        /**
         * Stream a full snapshot directly into OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        snapshot_full_to_opfs?(path: string): Promise<void>;
        /**
         * Stream a dirty-page snapshot directly into OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        snapshot_dirty_to_opfs?(path: string): Promise<void>;
        /**
         * Restore a snapshot by streaming it from OPFS using a `FileSystemSyncAccessHandle`.
         *
         * Note: this requires running in a Dedicated Worker (sync access handles are worker-only).
         */
        restore_snapshot_from_opfs?(path: string): Promise<void>;
        free(): void;
    };

    // Optional audio exports (present when the WASM build includes the audio worklet bridge).
    WorkletBridge?: new (capacityFrames: number, channelCount: number) => {
        readonly shared_buffer: SharedArrayBuffer;
        readonly capacity_frames: number;
        readonly channel_count: number;
        write_f32_interleaved(samples: Float32Array): number;
        buffer_level_frames(): number;
        underrun_count(): number;
        overrun_count(): number;
        free(): void;
    };
    create_worklet_bridge?: (capacityFrames: number, channelCount: number) => unknown;
    attach_worklet_bridge?: (sab: SharedArrayBuffer, capacityFrames: number, channelCount: number) => unknown;
    MicBridge?: {
        fromSharedBuffer(sab: SharedArrayBuffer): MicBridgeHandle;
    };
    attach_mic_bridge?: (sab: SharedArrayBuffer) => MicBridgeHandle;
    SineTone?: new () => {
        write(
            bridge: unknown,
            frames: number,
            freqHz: number,
            sampleRate: number,
            gain: number,
        ): number;
    };
    HdaPcmWriter?: new (dstSampleRateHz: number) => {
        readonly dst_sample_rate_hz: number;
        set_dst_sample_rate_hz(dstSampleRateHz: number): void;
        reset(): void;
        push_hda_pcm_bytes(bridge: unknown, hdaFormat: number, pcmBytes: Uint8Array): number;
        free(): void;
    };
    HdaPlaybackDemo?: new (
        ringSab: SharedArrayBuffer,
        capacityFrames: number,
        channelCount: number,
        hostSampleRate: number,
    ) => {
        readonly host_sample_rate_hz: number;
        readonly total_frames_produced: number;
        readonly total_frames_written: number;
        readonly total_frames_dropped: number;
        readonly last_tick_requested_frames: number;
        readonly last_tick_produced_frames: number;
        readonly last_tick_written_frames: number;
        readonly last_tick_dropped_frames: number;
        init_sine_dma(freqHz: number, gain: number): void;
        tick(frames: number): number;
        free(): void;
    };
}

export interface WasmInitMemoryInfo {
    byteLength: number;
    shared: boolean;
}

export interface WasmInitResult {
    api: WasmApi;
    variant: WasmVariant;
    reason: string;
    memory?: WasmInitMemoryInfo;
}

export interface WasmInitOptions {
    /**
     * - `auto` (default): pick the best variant for this runtime.
     * - `threaded`: require the shared-memory build and throw if unavailable.
     * - `single`: force the non-shared-memory build.
     */
    variant?: WasmVariant | "auto";

    /**
     * Optional precompiled WebAssembly.Module to instantiate from.
     *
     * When available, callers can compile the WASM binary once (on the main thread)
     * and structured-clone the resulting `WebAssembly.Module` into workers so each
     * context can instantiate without redundant compilation.
     *
     * If instantiation from the provided module fails, we fall back to wasm-bindgen's
     * default loader (fetch/compile) and log a warning.
     */
    module?: WebAssembly.Module;

    /**
     * Optional `WebAssembly.Memory` to use as the module's imported linear memory.
     *
     * This is required for worker/shared-memory mode: the coordinator allocates a
     * shared `WebAssembly.Memory` and workers inject it so both JS and WASM code
     * observe the same guest RAM.
     */
    memory?: WebAssembly.Memory;
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

function describeMemory(memory: WebAssembly.Memory | undefined): WasmInitMemoryInfo | undefined {
    if (!memory) return undefined;
    return {
        byteLength: memory.buffer.byteLength,
        shared: typeof SharedArrayBuffer !== "undefined" && memory.buffer instanceof SharedArrayBuffer,
    };
}

function toApi(mod: RawWasmModule): WasmApi {
    return {
        greet: mod.greet,
        add: mod.add,
        version: mod.version,
        sum: mod.sum,
        mem_store_u32: mod.mem_store_u32,
        guest_ram_layout: mod.guest_ram_layout,
        mem_load_u32: mod.mem_load_u32,
        demo_render_rgba8888: mod.demo_render_rgba8888,
        UsbHidBridge: mod.UsbHidBridge,
        CpuWorkerDemo: mod.CpuWorkerDemo,
        AeroApi: mod.AeroApi,
        DemoVm: mod.DemoVm,
        WorkletBridge: mod.WorkletBridge,
        create_worklet_bridge: mod.create_worklet_bridge,
        attach_worklet_bridge: mod.attach_worklet_bridge,
        MicBridge: mod.MicBridge,
        attach_mic_bridge: mod.attach_mic_bridge,
        SineTone: mod.SineTone,
        HdaPcmWriter: mod.HdaPcmWriter,
        HdaPlaybackDemo: mod.HdaPlaybackDemo,
    };
}

// `wasm-pack` outputs into `web/src/wasm/pkg-single` and `web/src/wasm/pkg-threaded`.
//
// These directories are generated (see `web/scripts/build_wasm.mjs`) and are not
// necessarily present in a fresh checkout. Use `import.meta.glob` so:
//  - Vite builds don't fail when the generated output is missing.
//  - When the output *is* present, it is bundled as usual.
const wasmImporters = import.meta.glob("../wasm/pkg-*/aero_wasm.js");

const warnOnceKeys = new Set<string>();
function warnOnce(key: string, message: string): void {
    if (warnOnceKeys.has(key)) return;
    warnOnceKeys.add(key);
    console.warn(message);
}

declare global {
    // eslint-disable-next-line no-var
    var __aeroWasmJsImporterOverride:
        | Partial<Record<WasmVariant, () => Promise<RawWasmModule>>>
        | undefined;
}

function resolveWasmImporter(variant: WasmVariant): (() => Promise<RawWasmModule>) | undefined {
    const override = globalThis.__aeroWasmJsImporterOverride?.[variant];
    if (override) return override;

    const path = variant === "single" ? "../wasm/pkg-single/aero_wasm.js" : "../wasm/pkg-threaded/aero_wasm.js";
    const importer = wasmImporters[path];
    if (!importer) return undefined;
    return importer as () => Promise<RawWasmModule>;
}

type WasmLoadResult = { api: WasmApi; memory?: WebAssembly.Memory };

function patchWasmImportsWithMemory(imports: unknown, memory: WebAssembly.Memory): void {
    if (!imports || typeof imports !== "object") return;
    const obj = imports as Record<string, any>;

    // `wasm-ld --import-memory` uses the canonical "env"."memory" import.
    obj.env ??= {};
    obj.env.memory = memory;

    // wasm-bindgen's JS glue also creates an `imports.wbg` (or
    // `imports.__wbindgen_placeholder__`) module for JS↔WASM shims; patch these
    // defensively in case the memory import gets routed differently by future
    // toolchains.
    if (obj.wbg && typeof obj.wbg === "object") obj.wbg.memory = memory;
    if (obj.__wbindgen_placeholder__ && typeof obj.__wbindgen_placeholder__ === "object") {
        obj.__wbindgen_placeholder__.memory = memory;
    }
}

async function withPatchedMemoryImport<T>(
    memory: WebAssembly.Memory,
    fn: () => Promise<T>,
): Promise<T> {
    const originalInstantiate = WebAssembly.instantiate;
    const hasInstantiateStreaming = typeof WebAssembly.instantiateStreaming === "function";
    const originalInstantiateStreaming = hasInstantiateStreaming ? WebAssembly.instantiateStreaming : undefined;

    // Avoid strict typing here: `WebAssembly.instantiate*` are overloaded and
    // Node/Vitest execute these `.ts` sources directly with stripped types.
    // Keep the runtime behavior correct while sidestepping TypeScript overload
    // assignability issues across TS versions.
    const instantiatePatched = (module: any, imports?: any) => {
        patchWasmImportsWithMemory(imports, memory);
        return originalInstantiate(module as any, imports as any);
    };

    (WebAssembly as any).instantiate = instantiatePatched;
    if (hasInstantiateStreaming) {
        const instantiateStreamingPatched = (source: any, imports?: any) => {
            patchWasmImportsWithMemory(imports, memory);
            return originalInstantiateStreaming!(source as any, imports as any);
        };
        (WebAssembly as any).instantiateStreaming = instantiateStreamingPatched;
    }
    try {
        return await fn();
    } finally {
        (WebAssembly as any).instantiate = originalInstantiate;
        if (hasInstantiateStreaming) {
            (WebAssembly as any).instantiateStreaming = originalInstantiateStreaming;
        }
    }
}

async function resolveWasmInputForInit(wasmUrl: URL): Promise<unknown> {
    // wasm-bindgen's `--target web` glue uses `fetch(new URL(..., import.meta.url))`.
    // In Node (Vitest) `fetch(file://...)` is not supported, so we pre-read the
    // `.wasm` bytes from disk and pass them to the init function directly.
    if (wasmUrl.protocol === "file:") {
        // Keep the dynamic imports opaque to Vite/Rollup so browser builds don't
        // try to resolve Node builtins.
        const fsPromises = "node:fs/promises";
        const nodeUrl = "node:url";
        const { readFile } = await import(/* @vite-ignore */ fsPromises);
        const { fileURLToPath } = await import(/* @vite-ignore */ nodeUrl);
        return await readFile(fileURLToPath(wasmUrl));
    }
    return undefined;
}

async function initWasmBindgenModule(
    mod: RawWasmModule,
    wasmUrl: URL,
    options: { variant: WasmVariant; module?: WebAssembly.Module; memory?: WebAssembly.Memory },
): Promise<void> {
    const initFn = mod.default;
    if (typeof initFn !== "function") {
        throw new Error("WASM package does not export a default wasm-bindgen init function.");
    }

    const { module, memory, variant } = options;
    let urlInputPromise: Promise<unknown> | null = null;
    const resolveUrlInput = async (): Promise<unknown> => {
        if (!urlInputPromise) {
            urlInputPromise = resolveWasmInputForInit(wasmUrl);
        }
        return await urlInputPromise;
    };

    const doInit = async (input: unknown): Promise<void> => {
        if (!memory) {
            await initFn(input);
            return;
        }

        // wasm-bindgen's `--import-memory` builds typically expose an init signature
        // like `default(input?, memory?)`. Unfortunately, the exact generated
        // signature has varied across wasm-bindgen versions.
        //
        // Robust strategy:
        // - Prefer calling `default(undefined, memory)` (or `default(input, memory)`
        //   in Node where we must pass bytes to avoid `fetch(file://...)`).
        // - Regardless of the signature, ensure the final instantiation sees the
        //   desired memory by patching `WebAssembly.instantiate*` to force
        //   `imports.env.memory = memory` before instantiation.
        await withPatchedMemoryImport(memory, async () => {
            try {
                // Preferred call shape (wasm-bindgen import-memory builds often use
                // `default(input?, memory?)`).
                await initFn(input, memory);
            } catch (err) {
                // Some wasm-bindgen outputs ignore/validate the extra argument.
                // Retrying with `default(input)` still uses the patched import object
                // (so the provided memory is wired up) but avoids "wrong number of
                // arguments" style failures.
                try {
                    await initFn(input);
                } catch {
                    throw err;
                }
            }
        });
    };

    if (module) {
        try {
            await doInit(module);
            return;
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            warnOnce(
                `wasm:init:${variant}:module`,
                `[wasm] ${variant} init with precompiled module failed; falling back to default loader. Error: ${message}`,
            );
            await doInit(await resolveUrlInput());
            return;
        }
    }

    await doInit(await resolveUrlInput());
}

function tryExtractWasmMemory(mod: RawWasmModule): WebAssembly.Memory | undefined {
    const candidates = [mod.memory, mod.__wbg_memory, mod.__wbindgen_memory, mod.wasm?.memory];
    for (const c of candidates) {
        if (c instanceof WebAssembly.Memory) return c;
    }
    return undefined;
}

async function loadSingle(options: WasmInitOptions): Promise<WasmLoadResult> {
    const importer = resolveWasmImporter("single");
    if (!importer) {
        throw new Error(
            [
                "Missing single-thread WASM package.",
                "",
                "Build it with:",
                "  cd web",
                "  npm run wasm:build:single",
                "Or a faster dev build:",
                "  npm run wasm:build:dev",
                "",
                "Or build both variants:",
                "  npm run wasm:build",
            ].join("\n"),
        );
    }
    const mod = (await importer()) as RawWasmModule;
    const wasmUrl = new URL("../wasm/pkg-single/aero_wasm_bg.wasm", import.meta.url);
    await initWasmBindgenModule(mod, wasmUrl, { variant: "single", module: options.module, memory: options.memory });
    return { api: toApi(mod), memory: options.memory ?? tryExtractWasmMemory(mod) };
}

async function loadThreaded(options: WasmInitOptions): Promise<WasmLoadResult> {
    const importer = resolveWasmImporter("threaded");
    if (!importer) {
        throw new Error(
            [
                "Missing threaded WASM package.",
                "",
                "Build it with:",
                "  cd web",
                "  npm run wasm:build:threaded",
                "Or a faster dev build:",
                "  node ./scripts/build_wasm.mjs threaded dev",
                "",
                "Or build both variants:",
                "  npm run wasm:build",
            ].join("\n"),
        );
    }
    const mod = (await importer()) as RawWasmModule;
    const wasmUrl = new URL("../wasm/pkg-threaded/aero_wasm_bg.wasm", import.meta.url);
    await initWasmBindgenModule(mod, wasmUrl, {
        variant: "threaded",
        module: options.module,
        memory: options.memory,
    });
    return { api: toApi(mod), memory: options.memory ?? tryExtractWasmMemory(mod) };
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

        const loaded = await loadThreaded(options);
        return {
            api: loaded.api,
            variant: "threaded",
            reason: threadSupport.reason,
            memory: describeMemory(loaded.memory),
        };
    }

    if (requested === "single") {
        const loaded = await loadSingle(options);
        return {
            api: loaded.api,
            variant: "single",
            reason: "Forced via initWasm({ variant: 'single' })",
            memory: describeMemory(loaded.memory),
        };
    }

    if (threadSupport.supported) {
        try {
            const loaded = await loadThreaded(options);
            return {
                api: loaded.api,
                variant: "threaded",
                reason: threadSupport.reason,
                memory: describeMemory(loaded.memory),
            };
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            const loaded = await loadSingle(options);
            return {
                api: loaded.api,
                variant: "single",
                reason: `Threaded WASM init failed; falling back to single. Error: ${message}`,
                memory: describeMemory(loaded.memory),
            };
        }
    }

    const loaded = await loadSingle(options);
    return { api: loaded.api, variant: "single", reason: threadSupport.reason, memory: describeMemory(loaded.memory) };
}
