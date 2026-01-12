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
| `kind` | `u32` | Application-defined (see §3.3) |
| `offset_bytes` | `u32` | Byte offset from start of buffer to the ring buffer header |
| `capacity_bytes` | `u32` | Size of the ring **data** region (excludes ring header) |
| `reserved` | `u32` | Must be `0` |

The descriptor makes layouts extensible: multiple queues can live in one `SharedArrayBuffer` without hard-coded offsets.

### 3.3 Queue kinds (application-defined)

The `kind` field is intentionally **application-defined**. The AIPC header/descriptor format only
describes **where** queues live, not what they mean.

Queue-kind values are therefore part of the application ABI for a particular shared buffer layout.
For the browser runtime’s `ioIpcSab` segment (created by
`web/src/runtime/shared_layout.ts:createIoIpcSab()`), the following kinds are currently assigned:

| Kind | Value (TS constant) | Producer → Consumer | Record payload |
|---|---:|---|---|
| `CMD` | `0` (`IO_IPC_CMD_QUEUE_KIND`) | CPU/WASM → IO worker | encoded `Command` (see §7.1) |
| `EVT` | `1` (`IO_IPC_EVT_QUEUE_KIND`) | IO worker → CPU/WASM | encoded `Event` (see §7.2) |
| `NET_TX` | `2` (`IO_IPC_NET_TX_QUEUE_KIND`) | CPU/WASM → network forwarder (IO worker / net worker) | raw Ethernet frame bytes |
| `NET_RX` | `3` (`IO_IPC_NET_RX_QUEUE_KIND`) | network forwarder (IO worker / net worker) → CPU/WASM | raw Ethernet frame bytes |
| `HID_IN` | `4` (`IO_IPC_HID_IN_QUEUE_KIND`) | coordinator/main thread → IO worker | WebHID input report record (see below) |

The `NET_TX`/`NET_RX` rings are separate from the command/event rings so bulk frame traffic does not
starve low-latency device operations.

For Rust/WASM, the same kind numbers are also exposed as:

```rust
use aero_ipc::layout::io_ipc_queue_kind::{CMD, EVT, HID_IN, NET_TX, NET_RX};
```

#### NET_TX / NET_RX semantics

These queues carry **one Ethernet frame per record** (no additional framing; the record payload is
exactly the frame bytes as seen by the emulated NIC).

- **Expected max frame size:** `2048` bytes.
  - Producers should drop frames larger than this.
  - Rationale: matches the default L2 tunnel payload limit (`web/src/shared/l2TunnelProtocol.ts`) and
    keeps ring-buffer sizing predictable.
- **Ownership (directionality):**
  - `NET_TX`: produced by the guest/emulator side; consumed by the JS transport that forwards to the
    proxy.
  - `NET_RX`: produced by the JS transport (frames received from the proxy); consumed by the
    guest/emulator side.
- **Drop/backpressure:**
  - These rings are bounded. When `tryPush()` fails (queue full), the producer should treat the
    frame as **dropped** (best-effort) rather than blocking the emulator.
  - Consumers should drain promptly; persistent drops generally indicate the tunnel/transport cannot
    keep up.

One way to visualize the browser-side plumbing:

```
CPU/WASM worker  --NET_TX-->  JS net forwarder  --WebSocket/WebRTC-->  proxy
CPU/WASM worker  <--NET_RX--  JS net forwarder  <--WebSocket/WebRTC--  proxy
```

#### HID_IN semantics

The `HID_IN` ring is used to forward WebHID input reports from the coordinator (main thread) to the
I/O worker.

- Producer: coordinator/main thread.
- Consumer: IO worker.
- Record payload: a `HidInputReportRingRecord`:
  - `u32 magic` (`0x5244_4948`, `"HIDR"` little-endian)
  - `u32 version` (`1`)
  - `u32 device_id`
  - `u32 report_id`
  - `u32 ts_ms` (`0` means “absent”; otherwise milliseconds, truncated to `u32`)
  - `u32 len` (payload byte length)
  - `len` data bytes (HID report payload)

See `web/src/hid/hid_input_report_ring.ts`.

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
| `0x0102` | `PortRead` | `u32 id`, `u16 port`, `u32 size` |
| `0x0103` | `PortWrite` | `u32 id`, `u16 port`, `u32 size`, `u32 value` |
| `0x0104` | `DiskRead` | `u32 id`, `u64 disk_offset`, `u32 len`, `u64 guest_offset` |
| `0x0105` | `DiskWrite` | `u32 id`, `u64 disk_offset`, `u32 len`, `u64 guest_offset` |
 
Note: for `DiskRead`/`DiskWrite`, `guest_offset` is a **guest physical address** (GPA). On PC/Q35,
guest RAM is non-contiguous once it exceeds the PCIe ECAM base (`0xB000_0000`) due to the ECAM/PCI
hole + high-RAM remap above 4 GiB. Implementations must translate GPAs back into their backing store
before indexing a flat byte array.

### 7.2 Events (worker → coordinator)
 
| Tag | Name | Payload |
|---:|---|---|
| `0x1000` | `Ack` | `u32 seq` |
| `0x1100` | `MmioReadResp` | `u32 id`, `u32 len`, `len bytes data` |
| `0x1101` | `PortReadResp` | `u32 id`, `u32 value` |
| `0x1102` | `MmioWriteResp` | `u32 id` |
| `0x1103` | `PortWriteResp` | `u32 id` |
| `0x1104` | `DiskReadResp` | `u32 id`, `u8 ok`, `u32 bytes`, (`u32 error_code` if `ok==0`) |
| `0x1105` | `DiskWriteResp` | `u32 id`, `u8 ok`, `u32 bytes`, (`u32 error_code` if `ok==0`) |
| `0x1200` | `FrameReady` | `u64 frame_id` |
| `0x1300` | `IrqRaise` | `u8 irq` |
| `0x1301` | `IrqLower` | `u8 irq` |
| `0x1302` | `A20Set` | `u8 enabled` |
| `0x1303` | `ResetRequest` | none |
| `0x1400` | `Log` | `u8 level`, `u32 len`, `len bytes UTF-8 message` |
| `0x1500` | `SerialOutput` | `u16 port`, `u32 len`, `len bytes data` |
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
  
Disk I/O error codes (`Disk*Resp.error_code` when `ok==0`):
  
| Code | Meaning |
|---:|---|
| 1 | no active disk opened |
| 2 | guest memory range out of bounds |
| 3 | disk offset not representable as a JS safe integer |
| 4 | disk I/O failure (read/write threw) |
  
---
 
## 8. Reference implementations
 
- Rust: `crates/aero-ipc/src/{layout,ring,protocol}.rs`
- WASM + JS Atomics glue: `crates/aero-ipc/src/wasm.rs`
- TypeScript: `web/src/ipc/{layout,ring_buffer,protocol}.ts`
- TypeScript (layout builder/parser): `web/src/ipc/ipc.ts`
- Browser demo: `web/demo/ipc_demo.html`
 
