# 19 - IndexedDB storage story (async vs sync)

See also:

- [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md) (repo-wide disk/backend trait inventory + canonical trait guidance)

## Context / problem statement

In this repo there are **two different “storage stacks”**:

1. **The Rust disk + controller stack** (boot-critical, used by the Rust AHCI/IDE device models)
   - `crates/aero-storage::{StorageBackend, VirtualDisk}` are **synchronous** traits.
   - `crates/aero-devices-storage` (AHCI/IDE/ATAPI) expects a `Box<dyn VirtualDisk>` and performs
     **synchronous** reads/writes during command processing.
   - In the browser, the intended synchronous backend is **OPFS SyncAccessHandle** via
     `crates/aero-opfs` (`aero_opfs::OpfsByteStorage` / `aero_opfs::OpfsBackend`).

2. **Async browser storage utilities** (UI/disk manager/import/export/benchmarks)
   - `crates/st-idb` is an **async** IndexedDB-backed block store.
   - The web runtime already contains async disk abstractions (`web/src/storage/*`).

This creates an integration gap:

- IndexedDB is fundamentally **async**.
- The Rust controller path is **sync**.
- In a browser Worker, attempting to “pretend” IndexedDB is sync by blocking the thread will
  deadlock (details below).

There is also an easy-to-miss footgun today:

- `crates/aero-opfs` can fall back to `OpfsIndexedDbBackend`, but that backend is **async-only**
  and therefore **cannot** back `aero_storage::VirtualDisk` or the synchronous Rust controller path.

The goal of this doc is to make the IndexedDB story explicit so we do not carry an implied
“fallback backend” that cannot actually be used by the boot-critical Rust device models.

---

## Why a sync wrapper around IndexedDB deadlocks (in the same Worker)

IndexedDB operations complete by delivering events (`onsuccess` / `onerror`) to the **same
agent’s event loop** that initiated the request:

1. The code issues an IDB request (e.g. `store.get(key)`).
2. The browser schedules an event to run later on that Worker’s event loop.
3. The event fires and your callback resolves a Promise / wakes a future.

If a Rust synchronous API (like `StorageBackend::read_at`) tries to:

- start an IndexedDB request, and then
- **block the current thread** until the callback resolves,

the callback can never run because the Worker’s event loop is blocked by the “sync wait”.

This deadlock happens regardless of *how* you block:

- A busy loop/spin-wait prevents the event loop from running.
- `Atomics.wait()` blocks the Worker thread entirely (no event loop progress).
- `futures::executor::block_on()`-style loops can repeatedly poll a future, but the future
  will remain `Pending` forever because the wakeup is delivered by an IDB callback that can’t run.

The key rule:

> **You cannot synchronously wait for IndexedDB from the same Worker that must run the IndexedDB
> completion callbacks.**

The only way to provide a *synchronous* facade is to run IndexedDB on a **different** Worker (with
its own event loop) and use cross-worker signaling to wait for results (see Option C).

---

## Viable options for Aero

This section enumerates the realistic integration paths. The goal is not to pick the “perfect”
architecture, but to be explicit about constraints so `ST-005` doesn’t accidentally promise a
fallback that cannot back the Rust controller.

### A) Require OPFS for the Rust controller path; IndexedDB only for disk manager/UI/import/export

**Idea:** Keep `aero-storage` + Rust controllers synchronous and boot-critical; in browser builds,
only allow the Rust controller path when **OPFS SyncAccessHandle** is available. IndexedDB remains
available for:

- disk manager / UI metadata
- import/export staging areas
- snapshots saved outside the boot-critical hot path
- benchmarks / diagnostics

**Pros**

- Minimal architectural change; fits current Rust sync traits.
- Best performance (OPFS SyncAccessHandle is the intended fast path).
- Avoids a fragile “sync IndexedDB” hack that would deadlock.

**Cons / constraints**

- Requires OPFS (and specifically SyncAccessHandle) to run the Rust controller path.
- Browsers/contexts without OPFS SyncAccessHandle must use an alternate mode (or fail early with
  a clear error).
- `ST-005` cannot mean “plug IndexedDB into `aero_storage::VirtualDisk`” under this option; it must
  be scoped to non-controller uses (see recommendation below).

### B) Move disk controllers to an async worker (TS or Rust/WASM); keep IndexedDB async

**Idea:** Make the *controller* itself async so disk I/O can `await` IndexedDB/async OPFS directly
without blocking.

Concretely: AHCI/IDE command processing becomes an async state machine. When a command needs disk
data, it issues an async read and resumes later to DMA data into guest RAM and raise interrupts.

**Pros**

- IndexedDB fits naturally (no sync wrapper).
- Allows async OPFS APIs and network streaming to share the same model.

**Cons / constraints**

- Large rework of the Rust controller code, which is currently written assuming synchronous I/O.
- Requires careful design to preserve determinism and to avoid starving other device work.
- Complicates the “device model is a pure function of guest-visible state + sync backend” mental model.

### C) Dedicated async “storage worker” + sync RPC client in the controller worker (SAB + Atomics)

**Idea:** Keep the Rust controller code synchronous, but move IndexedDB access to a **separate**
Worker that is free to `await` IDB callbacks. The controller worker talks to it via a shared-memory
RPC protocol:

- Controller worker (sync): writes a request record into a shared ring buffer, then waits for a
  completion (e.g. `Atomics.wait` on a per-request slot).
- Storage worker (async): reads requests, performs async IndexedDB operations, writes results back,
  and signals completion.

**Pros**

- Preserves the synchronous Rust controller API surface (`VirtualDisk` stays sync).
- Allows IndexedDB to be a true fallback backend for the Rust stack (without deadlocking).
- Similar patterns already exist in Aero (SharedArrayBuffer rings for high-frequency IPC).

**Cons / constraints**

- Requires `SharedArrayBuffer` + `Atomics` (i.e. cross-origin isolation) for the sync wait.
- Requires designing and maintaining a new shared-memory RPC protocol (buffer ownership, backpressure,
  cancellation, timeouts, poisoning/recovery).
- Still adds cross-worker copy/serialization overhead for data buffers unless designed carefully.

### D) Introduce async traits in Rust (`AsyncStorageBackend` / async controllers)

**Idea:** Make `aero-storage` async end-to-end, and then adapt controllers (and any callers) to
`async fn read_at(...)` / `async fn write_at(...)`.

**Pros**

- Most “pure” from an API standpoint: async storage is represented as async storage.
- Removes the need for sync blocking and reduces the need for cross-worker sync RPC tricks.

**Cons / constraints**

- Large refactor across `crates/aero-storage`, `crates/aero-devices-storage`, tests, and any glue code.
- Requires an async runtime model inside the emulation workers.
- High risk of scope creep and integration churn during boot-critical bring-up.

---

## Recommendation (near-term)

**Choose Option A for the near term.**

Rationale:

- The Rust AHCI/IDE device models are on the Windows 7 boot-critical path. We should optimize for
  correctness and simplicity first.
- OPFS SyncAccessHandle is the only browser storage API that is both (a) performant enough for large
  random I/O and (b) *actually synchronous* inside a Worker.
- Making the Rust controller async (Option B/D) or introducing a new SAB-based storage RPC layer
  (Option C) are both real, but substantially larger projects than the intended scope of `ST-005`.

How this ties back to **`instructions/io-storage.md` ST-005 (IndexedDB fallback backend)**:

- **ST-005 should be treated as an async-only fallback backend** that is usable by the **web host
  layer / disk manager / import-export tooling**, *not* as an implementation of
  `aero_storage::StorageBackend` / `aero_storage::VirtualDisk`.
- If we later decide we need IndexedDB as a runtime fallback for the Rust controller path, that is
  a separate milestone and likely maps to **Option C**.

---

## Follow-up implementation tasks (for Option A)

Concrete tasks to make this decision “real” in the codebase (so future work doesn’t accidentally
reintroduce an impossible fallback):

1. **Make OPFS SyncAccessHandle a hard requirement for the Rust controller runtime**
   - Web runtime: gate the “Rust controller” boot path on SyncAccessHandle availability and fail
     early with a clear user-facing error.
   - Likely targets:
     - `web/src/runtime/coordinator.ts` (capability checks + selecting runtime mode)
     - `web/src/platform/features.ts` (feature report surface)

2. **Document in Rust APIs that IndexedDB does not implement the synchronous traits**
   - Add or strengthen doc-comments (no behavior changes) clarifying:
     - `crates/aero-storage/src/backend.rs` (`StorageBackend` is sync; IDB is async-only)
     - `crates/aero-opfs/src/io/storage/backends/opfs.rs` (`OpfsIndexedDbBackend` is async-only and
       cannot back `aero_storage::VirtualDisk`)

3. **Scope ST-005 to the async host layer**
   - Use `crates/st-idb` (Rust/wasm32) and/or `web/src/storage/indexeddb.ts` (TypeScript) for:
     - disk/image management UIs
     - import/export staging
     - snapshot storage (when not on the hot I/O path)
   - Ensure any UI/runtime “fallback” messaging is explicit that IndexedDB does **not** enable the
     synchronous Rust controller path.

4. **Add a “no implicit IndexedDB fallback for Rust controllers” test/guardrail**
   - A lightweight guardrail can be a unit/integration test that asserts the Rust controller
     boot path only accepts `aero_opfs::OpfsBackend`/`OpfsByteStorage` (or other truly synchronous
     backends), never `OpfsStorage::IndexedDb`.
   - Likely targets:
     - `crates/aero-devices-storage/tests/*` (controller integration tests)
     - Any future wasm boot harness once the Rust controller is wired into the web runtime.
