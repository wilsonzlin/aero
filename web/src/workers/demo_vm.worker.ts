/// <reference lib="webworker" />

// Compatibility entrypoint: the snapshot worker already implements a dedicated-worker DemoVm
// runtime + OPFS streaming RPC surface. We keep this thin wrapper so callers can use a stable
// `demo_vm.worker.ts` name without duplicating implementation.
import "./demo_vm_snapshot.worker";

