# Legacy (Obsolete): Experimental GPU Command ABI (retired)

> Status: **retired**. This ABI is not implemented by the current emulator device model
> and is not used by the Win7 AeroGPU WDDM driver stack.

This repository previously contained an experimental GPU command ABI used for early
host-side experiments. It has been retired so that new work converges on the single
canonical AeroGPU protocol defined by the versioned headers:

- `drivers/aerogpu/protocol/*` (source of truth)
- `emulator/protocol/aerogpu/*` (generated Rust/TypeScript mirrors)

If you are implementing AeroGPU, writing a Windows driver, or capturing/replaying GPU
traces, use the canonical A3A0 protocol (`drivers/aerogpu/protocol/README.md`) and do
not depend on any legacy experimental command ABIs.
