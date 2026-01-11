/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_control_proto.h"

static void test_pcm_info_req_packing(void)
{
    VIRTIO_SND_PCM_INFO_REQ req;
    NTSTATUS status;

    status = VirtioSndCtrlBuildPcmInfoReq(NULL);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmInfoReq(&req);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(sizeof(req) == 12);

    /* Little-endian wire encoding */
    {
        const uint8_t expected[] = {
            0x00, 0x01, 0x00, 0x00, /* code = 0x0100 */
            0x00, 0x00, 0x00, 0x00, /* start_id = 0 */
            0x02, 0x00, 0x00, 0x00, /* count = 2 */
        };
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }
}

static void test_pcm_set_params_req_packing_and_validation(void)
{
    VIRTIO_SND_PCM_SET_PARAMS_REQ req;
    NTSTATUS status;

    status = VirtioSndCtrlBuildPcmSetParamsReq(NULL, VIRTIO_SND_PLAYBACK_STREAM_ID, 4096u, 1024u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, VIRTIO_SND_PLAYBACK_STREAM_ID, 4096u, 1024u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_SET_PARAMS);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    TEST_ASSERT(req.buffer_bytes == 4096u);
    TEST_ASSERT(req.period_bytes == 1024u);
    TEST_ASSERT(req.features == 0u);
    TEST_ASSERT(req.channels == 2u);
    TEST_ASSERT(req.format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(req.rate == VIRTIO_SND_PCM_RATE_48000);
    TEST_ASSERT(req.padding == 0u);

    {
        const uint8_t expected[] = {
            0x01, 0x01, 0x00, 0x00, /* code = 0x0101 */
            0x00, 0x00, 0x00, 0x00, /* stream_id = 0 */
            0x00, 0x10, 0x00, 0x00, /* buffer_bytes = 4096 */
            0x00, 0x04, 0x00, 0x00, /* period_bytes = 1024 */
            0x00, 0x00, 0x00, 0x00, /* features = 0 */
            0x02, 0x05, 0x07, 0x00, /* channels/format/rate/padding */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    /* Capture stream is mono => 2 bytes/frame alignment. */
    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, 960u, 480u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    TEST_ASSERT(req.channels == 1u);

    /* Bad stream id */
    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, 2u, 960u, 480u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Misaligned buffer/period sizes */
    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, VIRTIO_SND_PLAYBACK_STREAM_ID, 3u, 2u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, 4u, 6u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
}

static void test_pcm_simple_req_packing(void)
{
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    NTSTATUS status;

    status = VirtioSndCtrlBuildPcmSimpleReq(NULL, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_PREPARE);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_PREPARE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_PREPARE);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);

    {
        const uint8_t expected[] = {
            0x02, 0x01, 0x00, 0x00, /* code = 0x0102 */
            0x00, 0x00, 0x00, 0x00, /* stream_id = 0 */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_START);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_STOP);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_STOP);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_RELEASE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_RELEASE);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, 0xDEADu);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
}

static void test_pcm_info_resp_parsing(void)
{
    uint8_t resp[sizeof(VIRTIO_SND_HDR_RESP) + (sizeof(VIRTIO_SND_PCM_INFO) * 2)];
    VIRTIO_SND_HDR_RESP hdr;
    VIRTIO_SND_PCM_INFO info0;
    VIRTIO_SND_PCM_INFO info1;
    VIRTIO_SND_PCM_INFO out0;
    VIRTIO_SND_PCM_INFO out1;
    NTSTATUS status;

    RtlZeroMemory(resp, sizeof(resp));

    status = VirtioSndCtrlParsePcmInfoResp(NULL, 0, &out0, &out1);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), NULL, &out1);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    hdr.status = VIRTIO_SND_S_OK;

    RtlZeroMemory(&info0, sizeof(info0));
    info0.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info0.direction = VIRTIO_SND_D_OUTPUT;
    info0.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info0.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    info0.channels_min = 2;
    info0.channels_max = 2;

    RtlZeroMemory(&info1, sizeof(info1));
    info1.stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    info1.direction = VIRTIO_SND_D_INPUT;
    info1.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info1.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    info1.channels_min = 1;
    info1.channels_max = 1;

    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));

    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(out0.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    TEST_ASSERT(out1.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);

    /* Non-OK status is mapped via VirtioSndStatusToNtStatus. */
    hdr.status = VIRTIO_SND_S_NOT_SUPP;
    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);

    hdr.status = VIRTIO_SND_S_BAD_MSG;
    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    hdr.status = VIRTIO_SND_S_IO_ERR;
    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_INVALID_DEVICE_STATE);

    /* Short response is rejected as protocol error. */
    hdr.status = VIRTIO_SND_S_OK;
    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)(sizeof(VIRTIO_SND_HDR_RESP) + sizeof(VIRTIO_SND_PCM_INFO)), &out0, &out1);
    TEST_ASSERT(status == STATUS_DEVICE_PROTOCOL_ERROR);

    /* Wrong stream ids are rejected as protocol error. */
    hdr.status = VIRTIO_SND_S_OK;
    info0.stream_id = 1234;
    RtlCopyMemory(resp, &hdr, sizeof(hdr));
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_DEVICE_PROTOCOL_ERROR);
}

int main(void)
{
    test_pcm_info_req_packing();
    test_pcm_set_params_req_packing_and_validation();
    test_pcm_simple_req_packing();
    test_pcm_info_resp_parsing();

    printf("virtiosnd_control_proto_tests: PASS\n");
    return 0;
}
