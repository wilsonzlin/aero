/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_control_proto.h"
#include "virtiosnd_limits.h"

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

    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        4096u,
        1024u);
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
    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_CAPTURE_STREAM_ID,
        960u,
        480u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    TEST_ASSERT(req.channels == 1u);
    {
        const uint8_t expected[] = {
            0x01, 0x01, 0x00, 0x00, /* code = 0x0101 */
            0x01, 0x00, 0x00, 0x00, /* stream_id = 1 */
            0xC0, 0x03, 0x00, 0x00, /* buffer_bytes = 960 */
            0xE0, 0x01, 0x00, 0x00, /* period_bytes = 480 */
            0x00, 0x00, 0x00, 0x00, /* features = 0 */
            0x01, 0x05, 0x07, 0x00, /* channels/format/rate/padding */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    /* Bad stream id */
    status = VirtioSndCtrlBuildPcmSetParamsReq(&req, 2u, 960u, 480u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Misaligned buffer/period sizes */
    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        3u,
        2u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_CAPTURE_STREAM_ID,
        4u,
        6u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Contract v1: a single PCM payload > 256 KiB must be rejected with BAD_MSG. */
    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES + 4u,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES + 4u);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    /* Boundary case: exactly 256 KiB is accepted (payload bytes, header/status excluded). */
    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndCtrlBuildPcmSetParamsReq(
        &req,
        VIRTIO_SND_CAPTURE_STREAM_ID,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES,
        VIRTIOSND_MAX_PCM_PAYLOAD_BYTES);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /*
     * Multi-format builder (non-contract): verify S24 uses 4 bytes/sample
     * (24-bit samples stored in a 32-bit container).
     */
    status = VirtioSndCtrlBuildPcmSetParamsReqEx(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        1920u,
        192u,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    TEST_ASSERT(req.channels == 2u);
    TEST_ASSERT(req.format == VIRTIO_SND_PCM_FMT_S24);
    TEST_ASSERT(req.rate == VIRTIO_SND_PCM_RATE_44100);

    /* Misaligned period (not divisible by 8 bytes/frame) must be rejected. */
    status = VirtioSndCtrlBuildPcmSetParamsReqEx(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        1920u,
        194u,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Unsupported format must be rejected. */
    status = VirtioSndCtrlBuildPcmSetParamsReqEx(
        &req,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        1920u,
        192u,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_IMA_ADPCM,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
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
    {
        const uint8_t expected[] = {
            0x04, 0x01, 0x00, 0x00, /* code = 0x0104 */
            0x01, 0x00, 0x00, 0x00, /* stream_id = 1 */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_PLAYBACK_STREAM_ID, VIRTIO_SND_R_PCM_STOP);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_STOP);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    {
        const uint8_t expected[] = {
            0x05, 0x01, 0x00, 0x00, /* code = 0x0105 */
            0x00, 0x00, 0x00, 0x00, /* stream_id = 0 */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, VIRTIO_SND_R_PCM_RELEASE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(req.code == VIRTIO_SND_R_PCM_RELEASE);
    TEST_ASSERT(req.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    {
        const uint8_t expected[] = {
            0x03, 0x01, 0x00, 0x00, /* code = 0x0103 */
            0x01, 0x00, 0x00, 0x00, /* stream_id = 1 */
        };
        TEST_ASSERT(sizeof(req) == sizeof(expected));
        TEST_ASSERT_MEMEQ(&req, expected, sizeof(expected));
    }

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, VIRTIO_SND_CAPTURE_STREAM_ID, 0xDEADu);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndCtrlBuildPcmSimpleReq(&req, 2u, VIRTIO_SND_R_PCM_PREPARE);
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

    /*
     * Multi-format negotiation:
     * The parser should accept responses that do not include the Aero contract v1
     * fixed format (S16/48kHz), as long as at least one supported tuple exists.
     */
    info0.formats = VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_S24);
    info0.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_SUCCESS);

    info0.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info0.rates = VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_44100);
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /* Restore contract default values for subsequent validation tests. */
    info0.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info0.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));

    /* Direction validation. */
    info0.direction = VIRTIO_SND_D_INPUT;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_DEVICE_PROTOCOL_ERROR);
    info0.direction = VIRTIO_SND_D_OUTPUT;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));

    /* Format/rate validation. */
    info0.formats = 0;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
    info0.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    RtlCopyMemory(resp + sizeof(hdr), &info0, sizeof(info0));

    info1.rates = 0;
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
    info1.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));

    /* Channel range validation (min > max is rejected). */
    info1.channels_min = 2;
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));
    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)sizeof(resp), &out0, &out1);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
    info1.channels_min = 1;
    RtlCopyMemory(resp + sizeof(hdr) + sizeof(info0), &info1, sizeof(info1));

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

static void test_pcm_info_resp_unaligned_buffer(void)
{
    /*
     * Ensure the parser does not assume alignment for the status field or
     * PCM_INFO entries (it uses RtlCopyMemory).
     */
    uint8_t raw[1 + sizeof(VIRTIO_SND_HDR_RESP) + (sizeof(VIRTIO_SND_PCM_INFO) * 2)];
    uint8_t* resp = raw + 1;
    VIRTIO_SND_HDR_RESP hdr;
    VIRTIO_SND_PCM_INFO info0;
    VIRTIO_SND_PCM_INFO info1;
    VIRTIO_SND_PCM_INFO out0;
    VIRTIO_SND_PCM_INFO out1;
    NTSTATUS status;

    RtlZeroMemory(raw, sizeof(raw));
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

    status = VirtioSndCtrlParsePcmInfoResp(resp, (ULONG)(sizeof(raw) - 1u), &out0, &out1);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(out0.stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    TEST_ASSERT(out1.stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
}

static void test_pcm_format_selection_matrix(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    RtlZeroMemory(&info, sizeof(info));
    info.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info.direction = VIRTIO_SND_D_OUTPUT;
    info.channels_min = 2;
    info.channels_max = 2;

    /* Exact S16/48k present => keep legacy default. */
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);

    /* S16 present but only 44.1kHz => pick S16/44.1k. */
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_44100;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_44100);

    /* 48kHz present but only S24/S32 => pick best alternative (S24/48k per policy). */
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S24 | VIRTIO_SND_PCM_FMT_MASK_S32;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S24);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);

    /* Channels fallback: pick the lowest supported channel count if preferred is out of range. */
    info.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info.direction = VIRTIO_SND_D_OUTPUT;
    info.channels_min = 4;
    info.channels_max = 4;
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 4);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);

    /* stream_id must match the requested StreamId parameter. */
    info.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info.direction = VIRTIO_SND_D_OUTPUT;
    info.channels_min = 2;
    info.channels_max = 2;
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_CAPTURE_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* direction must match the stream direction. */
    info.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info.direction = VIRTIO_SND_D_INPUT;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Capture stream selection uses 1 channel when available. */
    RtlZeroMemory(&info, sizeof(info));
    info.stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    info.direction = VIRTIO_SND_D_INPUT;
    info.channels_min = 1;
    info.channels_max = 2;
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_CAPTURE_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 1);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);

    /* Completely unsupported masks => fail. */
    info.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    info.direction = VIRTIO_SND_D_OUTPUT;
    info.channels_min = 2;
    info.channels_max = 2;
    info.formats = 0;
    info.rates = 0;
    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
}

int main(void)
{
    test_pcm_info_req_packing();
    test_pcm_set_params_req_packing_and_validation();
    test_pcm_simple_req_packing();
    test_pcm_info_resp_parsing();
    test_pcm_info_resp_unaligned_buffer();
    test_pcm_format_selection_matrix();

    printf("virtiosnd_control_proto_tests: PASS\n");
    return 0;
}
