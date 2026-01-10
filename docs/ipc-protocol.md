# IPC Protocol & Shared-Memory Layout
 
This document defines Aero’s **inter-thread IPC contract** shared by:
 
- TypeScript (coordinator + web workers)
- Rust compiled to WASM (running inside workers)
 
The goal is a **stable**, **binary**, **SharedArrayBuffer + Atomics** based protocol that supports very high message rates without postMessage overhead.
 
---
 
## 1. Terminology
 
- **Coordinator**: main-thread “hub” that spawns and controls workers.
- **Worker**: dedicated thread for CPU, GPU, I/O, JIT, etc.
- **Queue**: a bounded ring buffer inside a `SharedArrayBuffer`.
- **Record**: one variable-length, length-prefixed entry in a queue.
 
All integers are **little-endian**.
 
---
 
## 2. High-level topology
 
The default topology is **SPSC per worker** (no global MPSC required):
 
- `cmd[i]`: coordinator → worker[i]
- `evt[i]`: worker[i] → coordinator
 
However, the ring buffer primitive is **MPSC-safe** (multi-producer, single-consumer) via a reservation + commit scheme, so it can also back global event/log queues if desired.
 
---
 
## 3. Shared memory layout
 
### 3.1 Top-level header
 
At byte offset `0` of the shared buffer:
 
| Field | Type | Description |
|---|---:|---|
| `magic` | `u32` | `0x4350_4941` (`"AIPC"` little-endian) |
| `version` | `u32` | ABI version (`1`) |
| `total_bytes` | `u32` | Total size of the shared buffer |
| `queue_count` | `u32` | Number of queue descriptors following |
 
Immediately after the header are `queue_count` queue descriptors.
 
### 3.2 Queue descriptor
 
Each descriptor is 16 bytes:
 
| Field | Type | Description |
|---|---:|---|
| `kind` | `u32` | Application-defined (`0=cmd`, `1=evt` in the default layout) |
| `offset_bytes` | `u32` | Byte offset from start of buffer to the ring buffer header |
| `capacity_bytes` | `u32` | Size of the ring **data** region (excludes ring header) |
| `reserved` | `u32` | Must be `0` |
 
The descriptor makes layouts extensible: multiple queues can live in one `SharedArrayBuffer` without hard-coded offsets.
 
---
 
## 4. Ring buffer layout
 
Each queue region begins with a 16-byte ring header, followed by the byte data region.
 
### 4.1 Ring header (`Int32Array[4]`)
 
The ring header must be accessed via an `Int32Array` so it can be driven by `Atomics.*`.
 
| Index | Name | Type | Description |
|---:|---|---:|---|
| 0 | `head` | `u32` | Consumer cursor (byte offset) |
| 1 | `tail_reserve` | `u32` | Producer reservation cursor (byte offset) |
| 2 | `tail_commit` | `u32` | Producer commit cursor (byte offset) |
| 3 | `capacity` | `u32` | Size of data region in bytes (written once at init) |
 
**Important:** cursors are stored as **wrapping `u32`**, but must be read/written through `Int32Array` (they may appear negative when viewed as signed).
 
### 4.2 Data region
 
The data region is a `Uint8Array(capacity)`.
 
The buffer stores **variable-length records**; each record is **contiguous** (never split across the end of the ring).
 
---
 
## 5. Record format (variable-length)
 
Each record is:
 
```
0:  u32 payload_len
4:  payload bytes (payload_len bytes)
..: padding (0–3 bytes) to 4-byte alignment
```
 
Alignment is currently **4 bytes** (`RECORD_ALIGN = 4`).
 
### 5.1 Wrap marker
 
If there is insufficient contiguous space at the end of the buffer for a record, producers write a **wrap marker** at the current tail index and advance to the next segment start:
 
- `payload_len = 0xFFFF_FFFF` (`WRAP_MARKER`)
 
Consumers that see `WRAP_MARKER` advance `head` to the next segment start.
 
If fewer than 4 bytes remain at the end, producers/consumers treat those bytes as **implicit padding** (no marker is written/read).
 
---
 
## 6. Concurrency semantics (MPSC-safe)
 
### 6.1 Reservation
 
Producers reserve space by atomically CAS’ing `tail_reserve` forward by the number of bytes they will consume (including any padding + optional wrap marker).
 
### 6.2 Commit (publish)
 
After writing the record payload, producers **publish** by advancing `tail_commit` *in-order*:
 
- A producer waits until `tail_commit == reservation_start`
- Then stores `tail_commit = reservation_end`
- Then `Atomics.notify` on `tail_commit`
 
Consumers only treat the queue as non-empty when `head != tail_commit`, ensuring they never read uncommitted bytes.
 
### 6.3 Blocking wait/notify
 
When legal (worker contexts), wait/notify is:
 
- Consumer waits on `tail_commit` when empty.
- Producers may wait on `head` when full.
 
Browser main thread cannot call `Atomics.wait`; coordinators should use polling or `Atomics.waitAsync` (if available).
 
---
 
## 7. Message protocol (record payloads)
 
Records in **command** queues contain an encoded `Command`.
Records in **event** queues contain an encoded `Event`.
 
Each message begins with a 16-bit `tag` identifying the variant; the rest is variant-specific.
 
### 7.1 Commands (coordinator → worker)
 
| Tag | Name | Payload |
|---:|---|---|
| `0x0000` | `Nop` | `u32 seq` |
| `0x0001` | `Shutdown` | none |
| `0x0100` | `MmioRead` | `u32 id`, `u64 addr`, `u32 size` |
| `0x0101` | `MmioWrite` | `u32 id`, `u64 addr`, `u32 len`, `len bytes data` |
 
### 7.2 Events (worker → coordinator)
 
| Tag | Name | Payload |
|---:|---|---|
| `0x1000` | `Ack` | `u32 seq` |
| `0x1100` | `MmioReadResp` | `u32 id`, `u32 len`, `len bytes data` |
| `0x1200` | `FrameReady` | `u64 frame_id` |
| `0x1300` | `IrqRaise` | `u8 irq` |
| `0x1301` | `IrqLower` | `u8 irq` |
| `0x1400` | `Log` | `u8 level`, `u32 len`, `len bytes UTF-8 message` |
| `0x1FFE` | `Panic` | `u32 len`, `len bytes UTF-8 message` |
| `0x1FFF` | `TripleFault` | none |
 
Log levels:
 
| Value | Name |
|---:|---|
| 0 | trace |
| 1 | debug |
| 2 | info |
| 3 | warn |
| 4 | error |
 
---
 
## 8. Reference implementations
 
- Rust: `crates/aero-ipc/src/{layout,ring,protocol}.rs`
- WASM + JS Atomics glue: `crates/aero-ipc/src/wasm.rs`
- TypeScript: `web/src/ipc/{layout,ring_buffer,protocol}.ts`
- Browser demo: `web/demo/ipc_demo.html`
 
