/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_msix_wdm.h"

/*
 * Keep assert() active in all build configs (Release may define NDEBUG).
 */
#undef assert
#define assert(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                  \
            abort();                                                                                                     \
        }                                                                                                                \
    } while (0)

typedef struct msix_test_ctx {
    PVIRTIO_MSIX_WDM expected_msix;
    int config_calls;
    int drain_calls;
    int drain_calls_by_queue[8];
} msix_test_ctx_t;

static VOID evt_config(_In_ PDEVICE_OBJECT DeviceObject, _In_opt_ PVOID Cookie)
{
    msix_test_ctx_t* ctx = (msix_test_ctx_t*)Cookie;
    (void)DeviceObject;
    assert(ctx != NULL);
    assert(ctx->expected_msix != NULL);
    ctx->config_calls++;
}

static VOID evt_drain(_In_ PDEVICE_OBJECT DeviceObject, _In_ ULONG QueueIndex, _In_opt_ PVOID Cookie)
{
    msix_test_ctx_t* ctx = (msix_test_ctx_t*)Cookie;
    (void)DeviceObject;
    assert(ctx != NULL);
    assert(ctx->expected_msix != NULL);
    assert(QueueIndex < (ULONG)(sizeof(ctx->drain_calls_by_queue) / sizeof(ctx->drain_calls_by_queue[0])));
    ctx->drain_calls++;
    ctx->drain_calls_by_queue[QueueIndex]++;
}

static CM_PARTIAL_RESOURCE_DESCRIPTOR make_msg_desc(_In_ USHORT messageCount)
{
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    RtlZeroMemory(&desc, sizeof(desc));
    desc.Type = CmResourceTypeInterrupt;
    desc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    desc.u.MessageInterrupt.Vector = 0x20;
    desc.u.MessageInterrupt.Level = 0x5;
    desc.u.MessageInterrupt.Affinity = 0x1;
    desc.u.MessageInterrupt.MessageCount = messageCount;
    return desc;
}

static void test_connect_validation(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    desc = make_msg_desc(1);

    status = VirtioMsixConnect(&dev, NULL, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioMsixConnect(NULL, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    desc.Type = 0;
    status = VirtioMsixConnect(&dev, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    desc = make_msg_desc(1);
    desc.Flags = 0; /* not message-based */
    status = VirtioMsixConnect(&dev, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_NOT_SUPPORTED);

    desc = make_msg_desc(0);
    status = VirtioMsixConnect(&dev, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_DEVICE_CONFIGURATION_ERROR);

    /* Parameter validation failures must not call through to WDK interrupt routines. */
    assert(WdkTestGetIoConnectInterruptExCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
}

static void test_multivector_mapping(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    msix_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(3); /* enough for config + 2 queues */
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioMsixConnect(&dev, &desc, 2, NULL, evt_config, evt_drain, &ctx, &msix);
    assert(status == STATUS_SUCCESS);
    ctx.expected_msix = &msix;

    assert(msix.MessageCount == 3);
    assert(msix.UsedVectorCount == 3);
    assert(msix.MessageInfo != NULL);
    assert(msix.MessageInfo->MessageCount == 3);
    /* MessageData is an APIC vector in real systems; ensure it differs from message number indices. */
    assert(msix.MessageInfo->MessageInfo[0].MessageData == 0x50u);
    assert(msix.MessageInfo->MessageInfo[1].MessageData == 0x51u);
    assert(msix.MessageInfo->MessageInfo[2].MessageData == 0x52u);
    assert(msix.ConfigVector == 0);
    assert(msix.QueueVectors != NULL);
    assert(msix.QueueVectors[0] == 1);
    assert(msix.QueueVectors[1] == 2);

    /* Vector 0: config only. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[0].Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 0);

    /* Vector 1: queue 0 only. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 1) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[1].Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 1);
    assert(ctx.drain_calls_by_queue[0] == 1);
    assert(ctx.drain_calls_by_queue[1] == 0);

    VirtioMsixDisconnect(&msix);
}

static void test_all_on_0_fallback_drains_all_queues(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    msix_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(1); /* only one vector available */
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioMsixConnect(&dev, &desc, 2, NULL, evt_config, evt_drain, &ctx, &msix);
    assert(status == STATUS_SUCCESS);
    ctx.expected_msix = &msix;

    assert(msix.UsedVectorCount == 1);
    assert(msix.MessageInfo != NULL);
    assert(msix.MessageInfo->MessageCount == 1);
    assert(msix.MessageInfo->MessageInfo[0].MessageData == 0x50u);
    assert(msix.ConfigVector == 0);
    assert(msix.QueueVectors != NULL);
    assert(msix.QueueVectors[0] == 0);
    assert(msix.QueueVectors[1] == 0);

    /* Vector 0: config + all queues. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[0].Dpc) != FALSE);

    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 2);
    assert(ctx.drain_calls_by_queue[0] == 1);
    assert(ctx.drain_calls_by_queue[1] == 1);

    VirtioMsixDisconnect(&msix);
}

int main(void)
{
    test_connect_validation();
    test_multivector_mapping();
    test_all_on_0_fallback_drains_all_queues();

    printf("virtio_msix_wdm_tests: PASS\n");
    return 0;
}
