# Virtio PCI “modern” interrupt bring-up on Windows 7 (MSI-X vs INTx)

This guide is a *validation/debug checklist* for getting interrupts working for a virtio-pci **modern** device on **Windows 7**.
It focuses on the common “everything enumerates but no interrupts” / “INTx storm” class of failures, and on proving (with WinDbg + DbgView + driver prints) what Windows actually configured.

See also:

* [`../virtio/virtqueue-split-ring-win7.md`](../virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue algorithms (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
* [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.

Assumptions:

- You have a kernel debugger (WinDbg) attached or can capture `DbgPrint` output with **DbgView**.
- Your driver is WDM or KMDF (examples use KMDF naming, but the resource concepts are WDM).
- Your device is virtio-pci modern (has a `common_cfg` with `msix_config` and `queue_msix_vector`).

---

## 0. Quick mental model (what must line up)

For virtio-pci modern:

- Windows decides whether the device runs with **message-signaled interrupts** (MSI/MSI-X) or with **line-based INTx**.
- Your driver must:
  1. **Connect** whatever Windows assigned (INTx or MSI-X messages).
  2. If using MSI-X: **program virtio’s MSI-X vector selectors** (`common_cfg.msix_config`, `queue_msix_vector`) with the **MSI-X table entry indices** that correspond to the connected messages.
  3. If using INTx: **read the virtio ISR status register** in your ISR to deassert the line (or you’ll storm).

The debugging process below is about proving each link in that chain.

---

## 1) Confirm which interrupt mode Windows assigned (MSI-X vs INTx)

### 1.1 Device Manager → Resources tab (fast sanity check)

1. `devmgmt.msc` → find your device → **Properties** → **Resources**.
2. Look at **Interrupt Request** entries:
   - **INTx / line-based**: usually shows a small IRQ number (e.g. `IRQ 17`) and may be **shared** with other devices.
   - **Message signaled (MSI/MSI-X)**: typically shows one or *multiple* “Interrupt Request” entries with larger values (often shown in hex) and they’re usually **not shared**.

This is not perfect, but it’s a quick “are we even in the right mode?” check that works on Windows 7 without any special tooling.

### 1.2 Driver debug prints: detect `CM_RESOURCE_INTERRUPT_MESSAGE`

In `EvtDevicePrepareHardware` (KMDF) or `IRP_MN_START_DEVICE` (WDM), dump the translated resources. The key fact to print is whether the interrupt resource has the flag:

- `CM_RESOURCE_INTERRUPT_MESSAGE` set ⇒ **message-signaled** (MSI/MSI-X)
- not set ⇒ **INTx**

Also print the **message count** for message-signaled resources.

Minimal pseudo-code (WDM structs; names/fields are from WDK headers):

```c
static void DumpInterruptResource(_In_ PCM_PARTIAL_RESOURCE_DESCRIPTOR d)
{
    if (d->Type != CmResourceTypeInterrupt) return;

    const BOOLEAN isMessage = (d->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0;

    if (isMessage) {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL,
            "INT: MSI/MSI-X Level=%lu Vector=%lu Affinity=0x%Ix MessageCount=%u Flags=0x%x\n",
            d->u.MessageInterrupt.Level,
            d->u.MessageInterrupt.Vector,
            (ULONG_PTR)d->u.MessageInterrupt.Affinity,
            d->u.MessageInterrupt.MessageCount,
            d->Flags);
    } else {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL,
            "INT: INTx Level=%lu Vector=%lu Affinity=0x%Ix Flags=0x%x\n",
            d->u.Interrupt.Level,
            d->u.Interrupt.Vector,
            (ULONG_PTR)d->u.Interrupt.Affinity,
            d->Flags);
    }
}
```

What you’re looking for:

- If you think you’re using MSI-X but `CM_RESOURCE_INTERRUPT_MESSAGE` never appears, Windows gave you INTx.
- If `MessageCount` is 1 but you expected N queues + config, Windows only granted 1 message (often due to INF/registry policy or device capability limits).

### 1.3 WinDbg: use `!irp` (PnP start IRP) and `!pci` (MSI-X enable bit)

#### `!irp` (resources as Windows handed them to the driver)

If you can break in your driver at `IRP_MJ_PNP / IRP_MN_START_DEVICE` (WDM) you can inspect the PnP IRP directly:

1. Break on your PnP dispatch routine (x64 calling convention: `rdx` is the IRP pointer).
2. In WinDbg:

```text
!irp @rdx
```

Look for pointers to `AllocatedResources` and `AllocatedResourcesTranslated`, then dump them:

```text
dt nt!_CM_RESOURCE_LIST <AllocatedResourcesTranslated>
```

Inside the resource list, find `CmResourceTypeInterrupt` descriptors and check their `Flags` for `CM_RESOURCE_INTERRUPT_MESSAGE` and any `MessageCount`.

If you’re KMDF-only and don’t have a PnP dispatch routine, the most practical equivalent is to log the resource descriptors from `EvtDevicePrepareHardware` (section 1.2) and use WinDbg to verify the PCI config (next section).

#### `!pci` (verify MSI-X capability and whether it is enabled)

WinDbg can show the device’s PCI capabilities, including whether MSI-X is *enabled*.

1. Determine the device’s BDF (Bus/Device/Function). Device Manager → **Location information** often shows something like “PCI bus X, device Y, function Z”.
2. In WinDbg:

```text
.load pci
!pcitree
!pci <bus> <device> <function>
```

In the `!pci` output, look for:

- An **MSI-X capability** (table size, table/BIR location).
- **MSI-X Enable** state.

If your driver believes it has MSI-X messages but `!pci` shows MSI-X disabled, something is inconsistent (often a driver that never successfully started/connected interrupts, or programming happening at the wrong time).

---

## 2) Confirm MSI-X message count and message numbers actually connected

Windows “message interrupts” have two numbers that people often confuse:

- **MessageNumber** (what virtio wants): 0-based message ID / MSI-X table entry index.
- **Vector** (what the CPU sees): APIC/IDT vector used to deliver the interrupt.

For virtio, **you almost always want `MessageNumber`, not `Vector`.**

### 2.1 Log what KMDF connected: `WdfInterruptGetInfo`

After each `WdfInterruptCreate`, call `WdfInterruptGetInfo` and print:

- `MessageSignaled`
- `MessageNumber`
- `Vector`
- `Irql`
- `Affinity`

Example:

```c
static void DumpWdfInterrupt(_In_ WDFINTERRUPT Interrupt)
{
    WDF_INTERRUPT_INFO info;
    WDF_INTERRUPT_INFO_INIT(&info);
    WdfInterruptGetInfo(Interrupt, &info);

    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL,
        "WDFINT: MessageSignaled=%u MessageNumber=%lu Vector=%lu Irql=%u Affinity=0x%Ix\n",
        info.MessageSignaled,
        info.MessageNumber,
        info.Vector,
        info.Irql,
        (ULONG_PTR)info.Affinity);
}
```

What “good” looks like:

- `MessageSignaled=1`
- You see message numbers `0..(N-1)` across your created interrupts (or at least the subset you requested/connected).
- `Vector` values will look “random”/unrelated (often high) — that’s expected.

### 2.2 Cross-check with your ISR’s `MessageID`

KMDF’s ISR callback receives `MessageID`:

```c
BOOLEAN EvtInterruptIsr(WDFINTERRUPT Interrupt, ULONG MessageID);
```

For message-signaled interrupts, `MessageID` should match the interrupt’s `info.MessageNumber` (from `WdfInterruptGetInfo`). Logging this once (or sampling) is a good way to prove your ISR is running on the message you think it is.

---

## 3) Virtio-specific MSI-X vector programming checks (the “read-back or it didn’t happen” rule)

Virtio-pci modern has explicit vector selectors:

- `common_cfg.msix_config` (u16): which MSI-X vector is used for **config change** interrupts.
- `common_cfg.queue_msix_vector` (u16, per-queue via `queue_select`): which MSI-X vector is used for **queue** interrupts.

These fields take an MSI-X **table entry index** (0-based). In Windows terms, that is typically the **MessageNumber**.

### 3.1 Read back `common_cfg.msix_config`

After you decide which message will be the config interrupt:

1. Write `common_cfg.msix_config = <MessageNumber>`.
2. Read it back and log the value.

Expected:

- Read-back equals what you wrote.

If it reads back as `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`), see section 3.3.

### 3.2 Read back each queue’s `queue_msix_vector`

For each queue you expect to generate interrupts:

1. `common_cfg.queue_select = <queue index>`
2. Write `common_cfg.queue_msix_vector = <MessageNumber>`
3. Read back and log.

Expected:

- Read-back equals what you wrote (or equals the shared vector if you intentionally share).

### 3.3 What `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) means (and when it’s expected)

`0xFFFF` is `VIRTIO_PCI_MSI_NO_VECTOR` (“no vector assigned”).

You should treat it as:

- **Expected** immediately after device reset (virtio resets vectors to “no vector”).
- **Expected** if Windows assigned you INTx and MSI-X is not enabled/usable.
- **A hard error** if you think you are in MSI-X mode and you just programmed a vector:
  - You wrote an out-of-range vector index (common when mistakenly using APIC `Vector`).
  - MSI-X isn’t enabled at the PCI layer.
  - The device has fewer MSI-X table entries than you assumed.

For queue interrupts specifically: leaving `queue_msix_vector = VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) means *that queue will not interrupt* (you’ll either poll or never see completions depending on your design).

### 3.4 Reset sequencing: MSI-X vectors must be re-programmed after *every* reset

Virtio feature negotiation typically involves a reset (`device_status = 0`) at least once during bring-up.

Common pitfall:

- Vectors are programmed once early, then a reset happens, vectors revert to `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`), and from that point onward **no interrupts ever fire**.

Rule of thumb:

- Any code path that transitions the device through reset must also re-apply:
  - `msix_config`
  - every `queue_msix_vector` you rely on

…and verify via read-back.

---

## 4) Common failure modes (symptoms → proof → fix)

### 4.1 INTx storm because ISR status wasn’t read/cleared

**Symptoms**

- CPU pegged, system sluggish.
- ISR count increases extremely fast even when the device is “idle”.
- You see the ISR re-enter continuously (level-triggered line never deasserts).

**How to prove**

- Device Manager / driver resources show INTx (no `CM_RESOURCE_INTERRUPT_MESSAGE`).
- Your ISR never reads the virtio ISR status register (legacy ISR status capability).

**Fix**

- In INTx mode, always read the virtio ISR status register in the ISR as the *first* operation and treat it as the deassert/ack step.
- Avoid spamming `DbgPrint` from the ISR; use counters instead (section 5).

### 4.2 No interrupts because MSI-X vectors weren’t programmed (or were lost after reset)

**Symptoms**

- Windows reports message interrupts (resources show `CM_RESOURCE_INTERRUPT_MESSAGE`, `WdfInterruptGetInfo.MessageSignaled=1`).
- Your queues make progress only if you poll; otherwise completions never trigger an ISR/DPC.
- The ISR count stays at 0 even while you know the device should interrupt.

**How to prove**

- Read back `common_cfg.msix_config` and/or `queue_msix_vector` and they are still `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) after the device is “running”.
- You can correlate this with a recent reset/status transition in your logs.

**Fix**

- Program `msix_config` and each `queue_msix_vector` *after* the last reset and before enabling/using the queues.
- Read back and assert they are not `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`).

### 4.3 Wrong routing because the driver used APIC `Vector` instead of `MessageNumber`

**Symptoms**

- Driver logs show `Vector` values like `0xE1`, `0x93`, etc being written into virtio `msix_config` / `queue_msix_vector`.
- Read-back comes back `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) (device rejected the index) or interrupts never arrive.

**How to prove**

- Your `WdfInterruptGetInfo` prints show something like:
  - `MessageNumber=0` (or small)
  - `Vector=0xE1` (large)
- Your virtio read-back shows `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) after “programming”.

**Fix**

- Program virtio’s vector selectors with `MessageNumber` (MSI-X table entry index), not the CPU `Vector`.

---

## 5) Recommended minimal instrumentation (what to log, and where)

Use logging that is actionable but won’t destroy the machine when things go wrong.
On Windows 7, **DbgView** is a practical way to capture `DbgPrintEx` output without kernel debugging attached:

- Run DbgView as Administrator
- Enable **Capture Kernel**

### 5.1 At device start / `EvtDevicePrepareHardware`

Log once:

- Device identification (PCI location/BDF if you have it).
- Interrupt resources:
  - For each `CmResourceTypeInterrupt` descriptor:
    - whether `CM_RESOURCE_INTERRUPT_MESSAGE` is set
    - `MessageCount` (if message-based)
    - `Vector`, `Level`, `Affinity`

This answers: “Did Windows give me MSI-X or INTx, and how many messages?”

### 5.2 After connecting interrupts (each `WdfInterruptCreate`)

Log once per interrupt object:

- `WdfInterruptGetInfo` fields: `MessageNumber`, `Vector`, `Irql`, `Affinity`, `MessageSignaled`

This answers: “Which message numbers did KMDF actually connect?”

### 5.3 When programming virtio MSI-X vectors

For each write:

- What you wrote (`msix_config`, `queue_msix_vector`), and which message number you intended.
- The *read-back* value.

This answers: “Did the device accept my vector programming, or did it silently revert to `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`)?”

### 5.4 In the ISR and DPC: counters, not prints

Keep per-interrupt counters:

- `isr_count[message]++`
- `dpc_count[message]++`

Log only:

- Periodically (e.g., once per second in a timer, or every 4096 interrupts), print a summary:
  - counts
  - last-seen `MessageID` (for message interrupts)
  - for INTx: last ISR status value read (optional, sample it)

This answers: “Are interrupts firing? Are DPCs scheduled? Is one vector storming?”

---

## 6) Bring-up checklist (copy/paste)

1. **Device Manager → Resources**: verify whether you’re on MSI-X (message) or INTx.
2. Driver: dump translated interrupt resources; confirm `CM_RESOURCE_INTERRUPT_MESSAGE` and `MessageCount` if applicable.
3. Driver: for each `WDFINTERRUPT`, print `WdfInterruptGetInfo` and record **MessageNumber**.
4. WinDbg `!pci`: confirm MSI-X capability exists and MSI-X is enabled when you expect message interrupts.
5. Program virtio:
   - `common_cfg.msix_config = <MessageNumber>`
   - for each queue: `queue_msix_vector = <MessageNumber>`
6. Read back all virtio vector selectors:
   - if any are `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) unexpectedly, fix before chasing anything else.
7. Verify ISR/DPC counters increment when generating known events (queue kick/completion).
