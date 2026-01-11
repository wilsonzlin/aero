import type { WasmVariant } from "./wasm_loader";

const variantByModule = new WeakMap<WebAssembly.Module, WasmVariant>();

export function registerPrecompiledWasmModule(module: WebAssembly.Module, variant: WasmVariant): void {
  variantByModule.set(module, variant);
}

export function lookupPrecompiledWasmModuleVariant(module: WebAssembly.Module): WasmVariant | undefined {
  return variantByModule.get(module);
}

