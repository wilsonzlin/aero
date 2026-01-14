/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_control_proto.h"

static void init_playback_info(VIRTIO_SND_PCM_INFO* Info, UCHAR ChannelsMin, UCHAR ChannelsMax)
{
    RtlZeroMemory(Info, sizeof(*Info));
    Info->stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    Info->direction = VIRTIO_SND_D_OUTPUT;
    Info->channels_min = ChannelsMin;
    Info->channels_max = ChannelsMax;
}

static void init_capture_info(VIRTIO_SND_PCM_INFO* Info, UCHAR ChannelsMin, UCHAR ChannelsMax)
{
    RtlZeroMemory(Info, sizeof(*Info));
    Info->stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    Info->direction = VIRTIO_SND_D_INPUT;
    Info->channels_min = ChannelsMin;
    Info->channels_max = ChannelsMax;
}

static void test_select_exact_contract_default(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);
}

static void test_select_float_only_48000(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT);
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_FLOAT);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);
}

static void test_select_float_prefers_float_over_float64(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT) | VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT64);
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_FLOAT);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);
}

static void test_select_capture_float64_44100(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_capture_info(&info, 1, 1);
    info.formats = VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_FLOAT64);
    info.rates = VIRTIO_SND_PCM_RATE_MASK_44100;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_CAPTURE_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 1);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_FLOAT64);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_44100);
}

static void test_select_s16_only_5512(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_5512);

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_5512);
}

static void test_select_s16_only_44100(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_44100;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_44100);
}

static void test_select_48k_only_s24_s32(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S24 | VIRTIO_SND_PCM_FMT_MASK_S32;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S24);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);
}

static void test_select_s16_only_96000(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK(VIRTIO_SND_PCM_RATE_96000);

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 2);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_96000);
}

static void test_select_channels_fallback_to_mono(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    /* Preferred is stereo, but device only supports mono. */
    init_playback_info(&info, 1, 1);
    info.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(cfg.Channels == 1);
    TEST_ASSERT(cfg.Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(cfg.Rate == VIRTIO_SND_PCM_RATE_48000);
}

static void test_select_unsupported_formats_fail(void)
{
    VIRTIO_SND_PCM_INFO info;
    VIRTIOSND_PCM_CONFIG cfg;
    NTSTATUS status;

    init_playback_info(&info, 2, 2);
    info.formats = VIRTIO_SND_PCM_FMT_MASK(VIRTIO_SND_PCM_FMT_IMA_ADPCM);
    info.rates = VIRTIO_SND_PCM_RATE_MASK_48000;

    status = VirtioSndCtrlSelectPcmConfig(&info, VIRTIO_SND_PLAYBACK_STREAM_ID, &cfg);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
}

int main(void)
{
    test_select_exact_contract_default();
    test_select_float_only_48000();
    test_select_float_prefers_float_over_float64();
    test_select_capture_float64_44100();
    test_select_s16_only_5512();
    test_select_s16_only_44100();
    test_select_48k_only_s24_s32();
    test_select_s16_only_96000();
    test_select_channels_fallback_to_mono();
    test_select_unsupported_formats_fail();

    printf("virtiosnd_pcm_selection_tests: PASS\n");
    return 0;
}
