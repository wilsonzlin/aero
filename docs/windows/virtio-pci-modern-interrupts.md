# Virtio PCI (Modern) Interrupts on Windows 7 (KMDF)

This document is a **hands-on implementation guide** for wiring up interrupt handling in a Windows 7 **KMDF** driver for a **virtio-pci modern** device (Virtio 1.0+ PCI capabilities). It covers both:

* **MSI-X (message-signaled)** interrupts (preferred).
* **Legacy INTx (line-based)** interrupts (fallback).

The intent is that a virtio-input (or any virtio-pci modern) driver can implement interrupts correctly **without additional research**.

See also:

* [`../virtio/virtqueue-split-ring-win7.md`](../virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue algorithms (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
* [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.

## Terminology / constants (virtio + Windows)

* **INTx**: legacy PCI line interrupt (level-triggered, frequently shared).
* **MSI-X message** (Windows): one delivered interrupt “message”. In virtio MSI-X programming, this corresponds to an **MSI-X table entry index**.
* Virtio vector sentinel:
  * `VIRTIO_PCI_MSI_NO_VECTOR` = `0xFFFF` (means “no vector assigned”).
* Virtio ISR Status register (`VIRTIO_PCI_CAP_ISR_CFG`, 1 byte, read-to-clear):
  * bit 0 (`0x01`): a queue has pending work
  * bit 1 (`0x02`): configuration change

## 1) Enumerating interrupts in `EvtDevicePrepareHardware`

KMDF calls:

```c
NTSTATUS EvtDevicePrepareHardware(
    WDFDEVICE Device,
    WDFCMRESLIST ResourcesRaw,
    WDFCMRESLIST ResourcesTranslated
    );
```

You must locate interrupt resources in the translated list, determine whether you got MSI-X (message) or INTx (line), and capture the MSI-X message count.

### Canonical helper (recommended)

Aero’s Windows virtio drivers use a shared helper that implements the patterns in this
document:

* `drivers/windows/virtio/kmdf/virtio_pci_interrupts.{c,h}`

Key entry points:

* `VirtioPciInterruptsPrepareHardware(...)` — enumerates resources and creates the KMDF `WDFINTERRUPT` objects (MSI-X or INTx).
* `VirtioPciInterruptsProgramMsixVectors(...)` — programs `common_cfg.msix_config` / `queue_msix_vector` using the mapping chosen at prepare time.
* `VirtioPciInterruptsQuiesce(...)` / `VirtioPciInterruptsResume(...)` — implements the MSI-X reset/quiesce sequencing described in section 4.1.

Example usage (simplified):

```c
// EvtDevicePrepareHardware
status = VirtioPciInterruptsPrepareHardware(
    Device,
    &ctx->Interrupts,
    ResourcesRaw,
    ResourcesTranslated,
    ctx->NumQueues,
    ctx->Pci.IsrStatus,
    ctx->Pci.CommonCfgLock, // serializes queue_select sequences
    VirtioConfigChanged,
    VirtioDrainQueue,
    ctx);

// EvtDeviceD0Entry (after each virtio reset)
status = VirtioPciInterruptsProgramMsixVectors(&ctx->Interrupts, ctx->Pci.CommonCfg);

// Reset/reconfigure path (PASSIVE_LEVEL)
status = VirtioPciInterruptsQuiesce(&ctx->Interrupts, ctx->Pci.CommonCfg);
// ... reset device + re-init queues ...
status = VirtioPciInterruptsResume(&ctx->Interrupts, ctx->Pci.CommonCfg);
```

### Step-by-step: find `CmResourceTypeInterrupt`

1. Enumerate the **translated** resource list with `WdfCmResourceListGetCount/Descriptor`.
2. For each entry:
   * If `desc->Type == CmResourceTypeInterrupt`, it’s an interrupt resource.
3. Keep the **matching** raw descriptor at the same index (raw and translated lists are aligned by index for WDF).

### Step-by-step: distinguish MSI-X vs INTx

For a `CmResourceTypeInterrupt` descriptor:

* **Message-signaled (MSI/MSI-X)** if `(desc->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0`
* **Line-based (INTx)** otherwise

Notes:
* On Windows 7, message-signaled delivery is typically **opt-in via INF** (see section 5).
* Prefer MSI-X if both appear (uncommon, but drivers should handle it defensively).

### Step-by-step: obtain MSI-X message count (Win7 `CM_PARTIAL_RESOURCE_DESCRIPTOR`)

For message-signaled interrupts, Windows reports a single `CmResourceTypeInterrupt` descriptor whose union is **`u.MessageInterrupt`** (not `u.Interrupt`).

On Windows 7 WDK headers, the message count is:

```c
USHORT messageCount = descTranslated->u.MessageInterrupt.MessageCount;
```

Store this count in your device context; you will use it to decide whether you can afford:

* **1 vector** for config + **1 vector per queue**, or
* the fallback “everything on vector 0” mapping (section 3).

### Pseudocode: resource enumeration

```c
typedef struct _DEVICE_CONTEXT {
    // Interrupt “mode” selected from PnP resources.
    BOOLEAN UseMsix;

    // Number of MSI(-X) messages Windows granted for the device.
    USHORT MsixMessageCount; // 0 if not message-signaled

    // Indices into the CM resource lists.
    ULONG MsixResourceIndex; // valid if UseMsix
    ULONG IntxResourceIndex; // valid if !UseMsix

    // Interrupt objects (created from the resources above).
    WDFINTERRUPT IntxInterrupt;     // valid if !UseMsix
    WDFINTERRUPT* MsixInterrupts;   // array [MsixMessageCount] if UseMsix

    // Vector mapping policy for MSI-X.
    BOOLEAN MsixAllOnVector0; // TRUE when messages < (1 + NumQueues)

    // For INTx: stash ISR status bits for the DPC (ISR status is read-to-clear).
    volatile UCHAR PendingIsrStatus;
} DEVICE_CONTEXT;

NTSTATUS EvtDevicePrepareHardware(
    WDFDEVICE Device,
    WDFCMRESLIST Raw,
    WDFCMRESLIST Translated
    )
{
    DEVICE_CONTEXT* ctx = DeviceGetContext(Device);
    ctx->UseMsix = FALSE;
    ctx->MsixMessageCount = 0;

    ULONG count = WdfCmResourceListGetCount(Translated);
    for (ULONG i = 0; i < count; i++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR t = WdfCmResourceListGetDescriptor(Translated, i);
        if (!t) continue;

        if (t->Type != CmResourceTypeInterrupt) continue;

        const BOOLEAN isMessage =
            ((t->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0);

        if (isMessage) {
            ctx->UseMsix = TRUE;
            ctx->MsixResourceIndex = i;
            ctx->MsixMessageCount = t->u.MessageInterrupt.MessageCount;
        } else {
            // Only keep INTx if we didn’t find MSI-X (prefer MSI-X).
            if (!ctx->UseMsix) {
                ctx->IntxResourceIndex = i;
            }
        }
    }

    return STATUS_SUCCESS;
}
```

## 2) Creating KMDF interrupt objects

### Recommended MSI-X strategy: *one `WDFINTERRUPT` per message*

For virtio, a “one interrupt object per vector” model is convenient because:

* Each MSI-X vector gets its own **ISR + DPC callback**.
* Each DPC can have its own **context** (queue index, etc).
* You avoid a big “switch(MessageID)” ISR, and you can choose to parallelize per-queue work.

In KMDF, you do this by calling `WdfInterruptCreate` **once per message** with:

* `WDF_INTERRUPT_CONFIG.InterruptRaw` / `.InterruptTranslated` pointing at the **same** message interrupt resource descriptor.
* `WDF_INTERRUPT_CONFIG.MessageNumber = i` for message `i`.

> The important detail: the CM resource list gives you one message interrupt descriptor with `MessageCount`. You reuse it for each `WdfInterruptCreate`, changing only `MessageNumber`.

### INTx strategy: create a single `WDFINTERRUPT`

For INTx you create exactly one interrupt object, bound to the line-based descriptor. There is no message count, no `MessageNumber` to iterate.

### Pseudocode: creating interrupts (MSI-X and INTx)

```c
typedef enum _VIRTIO_INTERRUPT_KIND {
    VirtioInterruptConfig,
    VirtioInterruptQueue,
} VIRTIO_INTERRUPT_KIND;

typedef struct _INTERRUPT_CONTEXT {
    DEVICE_CONTEXT* Device;
    VIRTIO_INTERRUPT_KIND Kind;
    ULONG QueueIndex;   // valid if Kind == VirtioInterruptQueue
    ULONG MessageNumber; // 0..(MessageCount-1)
} INTERRUPT_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(INTERRUPT_CONTEXT, InterruptGetContext);

NTSTATUS CreateInterrupts(WDFDEVICE Device, WDFCMRESLIST Raw, WDFCMRESLIST Translated)
{
    DEVICE_CONTEXT* ctx = DeviceGetContext(Device);

    if (ctx->UseMsix) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc =
            WdfCmResourceListGetDescriptor(Raw, ctx->MsixResourceIndex);
        PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc =
            WdfCmResourceListGetDescriptor(Translated, ctx->MsixResourceIndex);

        // Create one WDFINTERRUPT per message Windows granted (0..MessageCount-1).
        //
        // NOTE: You can create fewer (e.g. only 0..required-1) if you know you will
        // never program higher vectors, but creating one-per-message keeps the
        // bookkeeping straightforward.
        ctx->MsixInterrupts = (WDFINTERRUPT*)ExAllocatePoolWithTag(
            NonPagedPool,
            sizeof(WDFINTERRUPT) * ctx->MsixMessageCount,
            'xMsV');
        if (!ctx->MsixInterrupts) return STATUS_INSUFFICIENT_RESOURCES;
        RtlZeroMemory(ctx->MsixInterrupts, sizeof(WDFINTERRUPT) * ctx->MsixMessageCount);

        for (ULONG m = 0; m < ctx->MsixMessageCount; m++) {
            WDF_INTERRUPT_CONFIG icfg;
            WDF_INTERRUPT_CONFIG_INIT(&icfg, VirtioMsixIsr, VirtioMsixDpc);
            icfg.InterruptRaw = rawDesc;
            icfg.InterruptTranslated = transDesc;
            icfg.MessageNumber = m;

            WDF_OBJECT_ATTRIBUTES attr;
            WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, INTERRUPT_CONTEXT);
            attr.ParentObject = Device;

            WDFINTERRUPT interrupt;
            NTSTATUS status = WdfInterruptCreate(Device, &icfg, &attr, &interrupt);
            if (!NT_SUCCESS(status)) return status;
            ctx->MsixInterrupts[m] = interrupt;

            INTERRUPT_CONTEXT* ictx = InterruptGetContext(interrupt);
            ictx->Device = ctx;
            ictx->MessageNumber = m;
            if (m == 0) {
                // Recommended mapping: message 0 is used for virtio config changes.
                ictx->Kind = VirtioInterruptConfig;
            } else {
                // Recommended mapping: message 1..N used for queue 0..(N-1).
                ictx->Kind = VirtioInterruptQueue;
                ictx->QueueIndex = m - 1;
            }
        }

        return STATUS_SUCCESS;
    }

    // INTx: exactly one interrupt object
    PCM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc =
        WdfCmResourceListGetDescriptor(Raw, ctx->IntxResourceIndex);
    PCM_PARTIAL_RESOURCE_DESCRIPTOR transDesc =
        WdfCmResourceListGetDescriptor(Translated, ctx->IntxResourceIndex);

    WDF_INTERRUPT_CONFIG icfg;
    WDF_INTERRUPT_CONFIG_INIT(&icfg, VirtioIntxIsr, VirtioIntxDpc);
    icfg.InterruptRaw = rawDesc;
    icfg.InterruptTranslated = transDesc;

    return WdfInterruptCreate(Device, &icfg, WDF_NO_OBJECT_ATTRIBUTES, &ctx->IntxInterrupt);
}
```

## 3) Programming virtio MSI-X vectors (`msix_config`, `queue_msix_vector`)

Virtio-pci modern uses **device registers** (in `common_cfg`) to route interrupt sources to MSI-X vectors:

* `common_cfg->msix_config` routes **configuration-change** interrupts.
* `common_cfg->queue_msix_vector` routes a specific **queue’s** interrupts (requires setting `queue_select` first).

### Mapping rule: Windows message number → virtio MSI-X vector

After creating a `WDFINTERRUPT` for message `i`, you can query:

```c
WDF_INTERRUPT_INFO info;
WDF_INTERRUPT_INFO_INIT(&info);
WdfInterruptGetInfo(Interrupt, &info);
ULONG message = info.MessageNumber; // 0-based
```

For MSI-X devices, `info.MessageNumber` is the **MSI-X table entry index** Windows assigned. That is the value you program into:

* `common_cfg->msix_config`
* `common_cfg->queue_msix_vector`

### Required vectors vs available messages

Virtio typically wants:

* 1 vector for config, plus
* 1 vector per virtqueue you enable

So:

```
required = 1 + number_of_queues
```

If Windows grants fewer than `required` messages, use the required fallback mapping:

* Route **config + all queues** to **vector 0**.

This guarantees:

* interrupts still work,
* the driver doesn’t need partial/per-queue MSI-X logic, and
* DPC(0) can simply “handle everything”.

### Read-back verification (device may return `VIRTIO_PCI_MSI_NO_VECTOR`)

After writing `msix_config` / `queue_msix_vector`, the driver should read back:

* If it reads back `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`), the device rejected the mapping (or MSI-X isn’t enabled).
* If it reads back something other than what you wrote, treat it as failure.

In either case, fall back to vector 0 mapping (or to INTx if you decide MSI-X is unusable).

### Pseudocode: vector programming (with fallback)

```c
#ifndef VIRTIO_PCI_MSI_NO_VECTOR
#define VIRTIO_PCI_MSI_NO_VECTOR ((USHORT)0xFFFF)
#endif

static USHORT MsixVectorFromWdfInterrupt(_In_ WDFINTERRUPT Interrupt)
{
    WDF_INTERRUPT_INFO info;
    WDF_INTERRUPT_INFO_INIT(&info);
    WdfInterruptGetInfo(Interrupt, &info);

    // For message-signaled interrupts, MessageNumber is the MSI-X table entry.
    if (info.MessageNumber >= VIRTIO_PCI_MSI_NO_VECTOR) {
        return VIRTIO_PCI_MSI_NO_VECTOR;
    }
    return (USHORT)info.MessageNumber;
}

VOID ProgramVirtioMsixVectors(DEVICE_CONTEXT* ctx)
{
    if (!ctx->UseMsix || ctx->MsixMessageCount == 0) {
        // INTx path: nothing to program in common_cfg for routing.
        return;
    }

    const USHORT required = (USHORT)(1 + ctx->NumQueues);
    const BOOLEAN canDoPerQueue = (ctx->MsixMessageCount >= required);

    // Recommended mapping:
    //   message 0 -> virtio config
    //   message 1.. -> virtqueue 0..
    const USHORT configVec = MsixVectorFromWdfInterrupt(ctx->MsixInterrupts[0]);
    if (configVec == VIRTIO_PCI_MSI_NO_VECTOR) goto fallback_to_vec0;

    ctx->common_cfg->msix_config = configVec;
    if (ctx->common_cfg->msix_config != configVec) goto fallback_to_vec0;

    for (USHORT q = 0; q < ctx->NumQueues; q++) {
        USHORT qVec = configVec;
        if (canDoPerQueue) {
            qVec = MsixVectorFromWdfInterrupt(ctx->MsixInterrupts[q + 1]);
        }

        ctx->common_cfg->queue_select = q;
        ctx->common_cfg->queue_msix_vector = qVec;

        const USHORT readback = ctx->common_cfg->queue_msix_vector;
        if (readback == VIRTIO_PCI_MSI_NO_VECTOR || readback != qVec) {
            goto fallback_to_vec0;
        }
    }

    // Also verify msix_config didn’t get cleared/reset under us.
    if (ctx->common_cfg->msix_config == VIRTIO_PCI_MSI_NO_VECTOR) {
        goto fallback_to_vec0;
    }

    // Driver should record whether vector 0 is “shared” or dedicated.
    ctx->MsixAllOnVector0 = !canDoPerQueue;
    return;

fallback_to_vec0:
    // Required fallback: route config + all queues to vector 0.
    //
    // This assumes you created message 0 and it corresponds to MSI-X table entry 0.
    ctx->common_cfg->msix_config = 0;
    for (USHORT q = 0; q < ctx->NumQueues; q++) {
        ctx->common_cfg->queue_select = q;
        ctx->common_cfg->queue_msix_vector = 0;
    }
    ctx->MsixAllOnVector0 = TRUE;
}
```

**Important sequencing note:** a virtio **device reset** (writing `device_status = 0`) can clear `msix_config` / `queue_msix_vector` back to `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`). If your driver resets the device in `EvtDeviceD0Entry`, you must call `ProgramVirtioMsixVectors` **after** each reset (see pitfalls).

## 4) ISR/DPC behavior: INTx vs MSI-X

### INTx: ISR must read `isr_status` to ACK/deassert

INTx is level-triggered: if you do not deassert the interrupt line, you can get an **interrupt storm**.

For virtio-pci INTx, “ACK/deassert” is performed by reading the **ISR Status** register (read-to-clear). Therefore the INTx ISR should:

1. Read `isr_status` (MMIO read of 1 byte)
2. If the value is `0`, return `FALSE` (not your interrupt, important for shared lines)
3. Otherwise queue the DPC and return `TRUE`

Pseudocode:

```c
BOOLEAN VirtioIntxIsr(WDFINTERRUPT Interrupt, ULONG MessageID)
{
    UNREFERENCED_PARAMETER(MessageID);

    DEVICE_CONTEXT* ctx = DeviceGetContext(WdfInterruptGetDevice(Interrupt));

    // Read-to-clear; also deasserts the INTx line.
    UCHAR isr = READ_REGISTER_UCHAR(ctx->isr_status_reg);
    if (isr == 0) {
        return FALSE; // shared interrupt, not ours
    }

    // Optionally stash bits for the DPC since the register is now cleared.
    ctx->PendingIsrStatus |= isr;

    WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

VOID VirtioIntxDpc(WDFINTERRUPT Interrupt, WDFOBJECT AssociatedObject)
{
    UNREFERENCED_PARAMETER(AssociatedObject);

    DEVICE_CONTEXT* ctx = DeviceGetContext(WdfInterruptGetDevice(Interrupt));
    UCHAR isr = ctx->PendingIsrStatus;
    ctx->PendingIsrStatus = 0;

    if (isr & 0x02) {
        VirtioHandleConfigChange(ctx);
    }
    if (isr & 0x01) {
        VirtioServiceAllQueues(ctx);
    }
}
```

### MSI-X: ISR should not depend on `isr_status`

With MSI-X:

* There is no level-triggered line to deassert.
* Windows delivers a specific message vector; use that to route work.
* You should not need `isr_status` for routing, and reading it can remove useful state if some other code expects it.

Pseudocode:

```c
BOOLEAN VirtioMsixIsr(WDFINTERRUPT Interrupt, ULONG MessageID)
{
    UNREFERENCED_PARAMETER(MessageID);
    WdfInterruptQueueDpcForIsr(Interrupt);
    return TRUE;
}

VOID VirtioMsixDpc(WDFINTERRUPT Interrupt, WDFOBJECT AssociatedObject)
{
    UNREFERENCED_PARAMETER(AssociatedObject);

    INTERRUPT_CONTEXT* ictx = InterruptGetContext(Interrupt);
    DEVICE_CONTEXT* ctx = ictx->Device;

    if (ctx->MsixAllOnVector0) {
        // Vector 0 is shared for config + all queues.
        VirtioHandleConfigChange(ctx);
        VirtioServiceAllQueues(ctx);
        return;
    }

    if (ictx->Kind == VirtioInterruptConfig) {
        VirtioHandleConfigChange(ctx);
    } else {
        VirtioServiceQueue(ctx, ictx->QueueIndex);
    }
}
```

## 4.1) MSI-X multi-vector concurrency model (and how to make it safe)

With MSI-X, Windows can deliver interrupts for different MSI-X messages to different CPUs. In KMDF, when you create **one `WDFINTERRUPT` per message**, each interrupt object has its own DPC — and **KMDF can run those DPCs concurrently**.

### IRQL summary

* **ISR (`EvtInterruptIsr`)** runs at **DIRQL**:
  * keep it minimal: no heavy locks, no virtqueue draining, no reset work
  * typically just queues the DPC (`WdfInterruptQueueDpcForIsr`)
* **DPC (`EvtInterruptDpc`)** runs at **DISPATCH_LEVEL**:
  * this is where virtqueue used-ring draining/completions run
  * multiple DPCs (different MSI-X messages) may run in parallel
* **PnP/power/reset** paths run at **PASSIVE_LEVEL**:
  * `EvtDevicePrepareHardware`, `EvtDeviceD0Entry/D0Exit`, IOCTL threads, etc.
  * these can race with DPC code unless explicitly synchronized

### What must be serialized (virtio-pci modern hazards)

1. **Per-virtqueue state + used-ring draining**
   * `last_used_idx`, in-flight descriptor tables, completion paths, etc.
   * must be protected against:
     * concurrent DPCs on other CPUs (other queues), and
     * submission/reset paths running outside the DPC
2. **Any sequence using `common_cfg.queue_select`**
   * `queue_select` is a single global register.
   * the sequence “write `queue_select`, then read/write queue-specific fields” must not interleave across threads.
3. **Device reset / MSI-X reprogramming vs active interrupts**
   * reset can clear vector registers back to `VIRTIO_PCI_MSI_NO_VECTOR`
   * interrupts must not run against partially initialized (or torn-down) queue state

### Locking scheme (recommended)

If you want MSI-X parallelism, do **not** rely on implicit framework serialization.

Use explicit locks instead:

* **Per-queue spinlock** (one per virtqueue)
  * guards queue state + used-ring draining
  * queue DPC must acquire this lock before touching the queue
* **Global `common_cfg` spinlock**
  * guards any access that writes `common_cfg.queue_select` and then accesses queue-specific common_cfg fields (`queue_msix_vector`, `queue_enable`, `queue_notify_off`, …)

Lock ordering (to avoid deadlocks if a path needs both):

1. acquire `common_cfg` lock
2. acquire per-queue lock

### KMDF: `WDF_INTERRUPT_CONFIG.AutomaticSerialization`

**Decision:** For MSI-X we intentionally set:

* `WDF_INTERRUPT_CONFIG.AutomaticSerialization = FALSE`

Rationale:

* With `AutomaticSerialization = TRUE`, KMDF often serializes ISR/DPC callbacks using the device synchronization scope, which can negate the performance benefit of per-queue MSI-X vectors.
* With it disabled, safety comes from the explicit spinlocks above.

### Reset / reprogramming safety sequence (MSI-X)

At PASSIVE_LEVEL (e.g. device reset / D0Exit):

1. Set an atomic `ResetInProgress = TRUE` flag (DPCs bail out early).
2. Disable OS interrupt delivery (`WdfInterruptDisable` on each interrupt object).
3. Disable device-side routing:
   * program `msix_config = VIRTIO_PCI_MSI_NO_VECTOR`
   * for each queue: `queue_select = q; queue_msix_vector = VIRTIO_PCI_MSI_NO_VECTOR`
4. Synchronize with in-flight DPC work:
   * acquire+release each queue lock once (waits for any running DPC to leave its critical section)
5. Reset / reinitialize device and queues.
6. Reprogram vectors (after reset) and verify read-back != `VIRTIO_PCI_MSI_NO_VECTOR`.
7. Re-enable OS interrupt delivery (`WdfInterruptEnable`).
8. Clear `ResetInProgress` only when queues are fully ready.

Concrete code implementing this model lives in:

* `drivers/windows/virtio/kmdf/virtio_pci_interrupts.c` (`VirtioPciInterruptsQuiesce` / `VirtioPciInterruptsResume`)

## 5) Windows 7 INF: enabling MSI/MSI-X

On Windows 7, message-signaled interrupts are typically enabled through INF registry settings under the device’s hardware key.

Minimum required:

```inf
[MyDevice_Install.NT.HW]
AddReg = MyDevice_AddReg

[MyDevice_AddReg]
HKR, "Interrupt Management\MessageSignaledInterruptProperties", "MSISupported", 0x00010001, 1
```

Optional: request up to N messages (Windows may grant fewer):

```inf
HKR, "Interrupt Management\MessageSignaledInterruptProperties", "MessageNumberLimit", 0x00010001, 8
```

Notes:
* `0x00010001` = `REG_DWORD`.
* Without `MSISupported=1`, you should expect only INTx resources in `EvtDevicePrepareHardware`.
* Setting `MessageNumberLimit` higher than what the virtio device exposes in its MSI-X table cannot succeed; always handle “granted < requested” at runtime.

## 6) Common pitfalls (checklist)

1. **INTx storm / hang**
   * Symptom: CPU stuck at high IRQL, ISR firing continuously.
   * Root cause: not reading `isr_status` in the INTx ISR (line never deasserts).
   * Fix: read `VIRTIO_PCI_CAP_ISR_CFG` in the ISR and return `FALSE` when it reads `0`.

2. **Using the wrong union fields for message interrupts**
   * Root cause: reading `desc->u.Interrupt.*` even when `CM_RESOURCE_INTERRUPT_MESSAGE` is set.
   * Fix: for message interrupts use `desc->u.MessageInterrupt.MessageCount` (Win7).

3. **Vector registers reset after virtio device reset**
   * Root cause: driver calls `device_status = 0` during init/recover but does not reprogram `msix_config` and `queue_msix_vector`.
   * Fix: call your MSI-X programming routine after every reset and before enabling queues/notifications.

4. **Assuming you always get (1 + numQueues) MSI-X messages**
   * Root cause: `MessageNumberLimit` is only a request; Windows can grant fewer messages.
   * Fix: implement the documented fallback mapping: config + all queues → vector 0.

5. **Concurrency surprises with “one WDFINTERRUPT per message”**
   * Multiple DPCs can run in parallel on different CPUs.
   * Per-interrupt locks (`WdfInterruptAcquireLock`) do **not** protect shared device/queue state across interrupts.
   * Fix: either:
     * add explicit locking around shared virtio state, or
     * configure the driver to serialize callbacks (device synchronization scope / interrupt serialization) and accept less parallelism.
