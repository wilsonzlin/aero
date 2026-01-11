#![no_std]
#![forbid(unsafe_code)]

/// Virtqueue descriptor table entry (`struct virtq_desc`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtqDesc {
  pub addr: u64,
  pub len: u32,
  pub flags: u16,
  pub next: u16,
}

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Virtqueue available ring header (does not include the variable-length ring array).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtqAvailHeader {
  pub flags: u16,
  pub idx: u16,
}

pub const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

/// Virtqueue used ring header (does not include the variable-length ring array).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtqUsedHeader {
  pub flags: u16,
  pub idx: u16,
}

pub const VIRTQ_USED_F_NO_NOTIFY: u16 = 1;

/// Virtqueue used ring element (`struct virtq_used_elem`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtqUsedElem {
  pub id: u32,
  pub len: u32,
}

/// PCI capability header for virtio-pci (`struct virtio_pci_cap`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioPciCap {
  pub cap_vndr: u8,
  pub cap_next: u8,
  pub cap_len: u8,
  pub cfg_type: u8,
  pub bar: u8,
  pub padding: [u8; 3],
  pub offset: u32,
  pub length: u32,
}

/// Notification capability (`struct virtio_pci_notify_cap`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioPciNotifyCap {
  pub cap: VirtioPciCap,
  pub notify_off_multiplier: u32,
}

/// Common configuration structure (`struct virtio_pci_common_cfg`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioPciCommonCfg {
  pub device_feature_select: u32,
  pub device_feature: u32,
  pub driver_feature_select: u32,
  pub driver_feature: u32,
  pub msix_config: u16,
  pub num_queues: u16,
  pub device_status: u8,
  pub config_generation: u8,
  pub queue_select: u16,
  pub queue_size: u16,
  pub queue_msix_vector: u16,
  pub queue_enable: u16,
  pub queue_notify_off: u16,
  pub queue_desc: u64,
  pub queue_avail: u64,
  pub queue_used: u64,
}

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
pub const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5;

pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
pub const VIRTIO_STATUS_FAILED: u8 = 128;

/// Virtio-blk request header (`struct virtio_blk_req` header portion).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioBlkReqHeader {
  pub req_type: u32,
  pub reserved: u32,
  pub sector: u64,
}

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// virtio-net header (`struct virtio_net_hdr`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioNetHdr {
  pub flags: u8,
  pub gso_type: u8,
  pub hdr_len: u16,
  pub gso_size: u16,
  pub csum_start: u16,
  pub csum_offset: u16,
}

/// virtio-net header used when `VIRTIO_NET_F_MRG_RXBUF` is negotiated
/// (`struct virtio_net_hdr_mrg_rxbuf`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioNetHdrMrgRxbuf {
  pub hdr: VirtioNetHdr,
  pub num_buffers: u16,
}

// virtio-snd (Paravirtual Audio) protocol.
pub const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
pub const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0101;
pub const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0102;
pub const VIRTIO_SND_R_PCM_RELEASE: u32 = 0x0103;
pub const VIRTIO_SND_R_PCM_START: u32 = 0x0104;
pub const VIRTIO_SND_R_PCM_STOP: u32 = 0x0105;

pub const VIRTIO_SND_S_OK: u32 = 0;
pub const VIRTIO_SND_S_BAD_MSG: u32 = 1;
pub const VIRTIO_SND_S_NOT_SUPP: u32 = 2;
pub const VIRTIO_SND_S_IO_ERR: u32 = 3;

pub const VIRTIO_SND_PCM_FMT_S16: u8 = 0x05;
pub const VIRTIO_SND_PCM_RATE_48000: u8 = 0x07;

pub const VIRTIO_SND_D_OUTPUT: u8 = 0x00;
pub const VIRTIO_SND_D_INPUT: u8 = 0x01;

pub const VIRTIO_SND_PCM_FMT_MASK_S16: u64 = 1u64 << VIRTIO_SND_PCM_FMT_S16;
pub const VIRTIO_SND_PCM_RATE_MASK_48000: u64 = 1u64 << VIRTIO_SND_PCM_RATE_48000;

pub const VIRTIO_SND_QUEUE_CONTROL: u16 = 0;
pub const VIRTIO_SND_QUEUE_EVENT: u16 = 1;
pub const VIRTIO_SND_QUEUE_TX: u16 = 2;
pub const VIRTIO_SND_QUEUE_RX: u16 = 3;

/// virtio-snd device configuration (`struct virtio_snd_config`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndConfig {
  pub jacks: u32,
  pub streams: u32,
  pub chmaps: u32,
}

/// virtio-snd PCM_INFO request (`struct virtio_snd_pcm_info_req`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndPcmInfoReq {
  pub code: u32,
  pub start_id: u32,
  pub count: u32,
}

/// virtio-snd PCM_SET_PARAMS request (`struct virtio_snd_pcm_set_params`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndPcmSetParamsReq {
  pub code: u32,
  pub stream_id: u32,
  pub buffer_bytes: u32,
  pub period_bytes: u32,
  pub features: u32,
  pub channels: u8,
  pub format: u8,
  pub rate: u8,
  pub padding: u8,
}

/// virtio-snd request containing only a stream id (`struct virtio_snd_pcm_hdr`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndPcmSimpleReq {
  pub code: u32,
  pub stream_id: u32,
}

/// virtio-snd PCM stream information (`struct virtio_snd_pcm_info`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndPcmInfo {
  pub stream_id: u32,
  pub features: u32,
  pub formats: u64,
  pub rates: u64,
  pub direction: u8,
  pub channels_min: u8,
  pub channels_max: u8,
  pub reserved: [u8; 5],
}

/// virtio-snd TX header (`struct virtio_snd_pcm_xfer` header portion).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndTxHdr {
  pub stream_id: u32,
  pub reserved: u32,
}

/// virtio-snd RX header (identical layout to [`VirtioSndTxHdr`]).
pub type VirtioSndRxHdr = VirtioSndTxHdr;

/// virtio-snd PCM status (`struct virtio_snd_pcm_status`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioSndPcmStatus {
  pub status: u32,
  pub latency_bytes: u32,
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
  use super::*;
  use core::mem::{offset_of, size_of};

  #[test]
  fn virtq_desc_layout() {
    assert_eq!(size_of::<VirtqDesc>(), 16);
    assert_eq!(offset_of!(VirtqDesc, addr), 0);
    assert_eq!(offset_of!(VirtqDesc, len), 8);
    assert_eq!(offset_of!(VirtqDesc, flags), 12);
    assert_eq!(offset_of!(VirtqDesc, next), 14);
  }

  #[test]
  fn virtq_avail_header_layout() {
    assert_eq!(size_of::<VirtqAvailHeader>(), 4);
    assert_eq!(offset_of!(VirtqAvailHeader, flags), 0);
    assert_eq!(offset_of!(VirtqAvailHeader, idx), 2);
  }

  #[test]
  fn virtq_used_header_and_elem_layout() {
    assert_eq!(size_of::<VirtqUsedHeader>(), 4);
    assert_eq!(size_of::<VirtqUsedElem>(), 8);
    assert_eq!(offset_of!(VirtqUsedElem, id), 0);
    assert_eq!(offset_of!(VirtqUsedElem, len), 4);
  }

  #[test]
  fn virtio_pci_cap_layout() {
    assert_eq!(size_of::<VirtioPciCap>(), 16);
    assert_eq!(offset_of!(VirtioPciCap, cap_vndr), 0);
    assert_eq!(offset_of!(VirtioPciCap, cfg_type), 3);
    assert_eq!(offset_of!(VirtioPciCap, bar), 4);
    assert_eq!(offset_of!(VirtioPciCap, offset), 8);
    assert_eq!(offset_of!(VirtioPciCap, length), 12);
  }

  #[test]
  fn virtio_pci_common_cfg_layout() {
    assert_eq!(size_of::<VirtioPciCommonCfg>(), 56);
    assert_eq!(offset_of!(VirtioPciCommonCfg, device_feature_select), 0);
    assert_eq!(offset_of!(VirtioPciCommonCfg, device_feature), 4);
    assert_eq!(offset_of!(VirtioPciCommonCfg, driver_feature), 12);
    assert_eq!(offset_of!(VirtioPciCommonCfg, msix_config), 16);
    assert_eq!(offset_of!(VirtioPciCommonCfg, device_status), 20);
    assert_eq!(offset_of!(VirtioPciCommonCfg, queue_notify_off), 30);
    assert_eq!(offset_of!(VirtioPciCommonCfg, queue_desc), 32);
    assert_eq!(offset_of!(VirtioPciCommonCfg, queue_used), 48);
  }

  #[test]
  fn virtio_blk_req_header_layout() {
    assert_eq!(size_of::<VirtioBlkReqHeader>(), 16);
    assert_eq!(offset_of!(VirtioBlkReqHeader, req_type), 0);
    assert_eq!(offset_of!(VirtioBlkReqHeader, sector), 8);
  }

  #[test]
  fn virtio_net_hdr_layout() {
    assert_eq!(size_of::<VirtioNetHdr>(), 10);
    assert_eq!(offset_of!(VirtioNetHdr, flags), 0);
    assert_eq!(offset_of!(VirtioNetHdr, hdr_len), 2);
  }

  #[test]
  fn virtio_net_hdr_mrg_rxbuf_layout() {
    assert_eq!(size_of::<VirtioNetHdrMrgRxbuf>(), 12);
    assert_eq!(offset_of!(VirtioNetHdrMrgRxbuf, hdr), 0);
    assert_eq!(offset_of!(VirtioNetHdrMrgRxbuf, num_buffers), 10);
  }

  #[test]
  fn virtio_snd_pcm_info_req_layout() {
    assert_eq!(size_of::<VirtioSndPcmInfoReq>(), 12);
    assert_eq!(offset_of!(VirtioSndPcmInfoReq, code), 0);
    assert_eq!(offset_of!(VirtioSndPcmInfoReq, start_id), 4);
    assert_eq!(offset_of!(VirtioSndPcmInfoReq, count), 8);
  }

  #[test]
  fn virtio_snd_pcm_set_params_req_layout() {
    assert_eq!(size_of::<VirtioSndPcmSetParamsReq>(), 24);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, code), 0);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, stream_id), 4);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, buffer_bytes), 8);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, period_bytes), 12);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, features), 16);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, channels), 20);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, format), 21);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, rate), 22);
    assert_eq!(offset_of!(VirtioSndPcmSetParamsReq, padding), 23);
  }

  #[test]
  fn virtio_snd_pcm_simple_req_layout() {
    assert_eq!(size_of::<VirtioSndPcmSimpleReq>(), 8);
    assert_eq!(offset_of!(VirtioSndPcmSimpleReq, code), 0);
    assert_eq!(offset_of!(VirtioSndPcmSimpleReq, stream_id), 4);
  }

  #[test]
  fn virtio_snd_pcm_info_layout() {
    assert_eq!(size_of::<VirtioSndPcmInfo>(), 32);
    assert_eq!(offset_of!(VirtioSndPcmInfo, stream_id), 0);
    assert_eq!(offset_of!(VirtioSndPcmInfo, features), 4);
    assert_eq!(offset_of!(VirtioSndPcmInfo, formats), 8);
    assert_eq!(offset_of!(VirtioSndPcmInfo, rates), 16);
    assert_eq!(offset_of!(VirtioSndPcmInfo, direction), 24);
    assert_eq!(offset_of!(VirtioSndPcmInfo, channels_min), 25);
    assert_eq!(offset_of!(VirtioSndPcmInfo, channels_max), 26);
    assert_eq!(offset_of!(VirtioSndPcmInfo, reserved), 27);
  }

  #[test]
  fn virtio_snd_tx_hdr_layout() {
    assert_eq!(size_of::<VirtioSndTxHdr>(), 8);
    assert_eq!(offset_of!(VirtioSndTxHdr, stream_id), 0);
    assert_eq!(offset_of!(VirtioSndTxHdr, reserved), 4);
  }

  #[test]
  fn virtio_snd_config_layout() {
    assert_eq!(size_of::<VirtioSndConfig>(), 12);
    assert_eq!(offset_of!(VirtioSndConfig, jacks), 0);
    assert_eq!(offset_of!(VirtioSndConfig, streams), 4);
    assert_eq!(offset_of!(VirtioSndConfig, chmaps), 8);
  }

  #[test]
  fn virtio_snd_pcm_status_layout() {
    assert_eq!(size_of::<VirtioSndPcmStatus>(), 8);
    assert_eq!(offset_of!(VirtioSndPcmStatus, status), 0);
    assert_eq!(offset_of!(VirtioSndPcmStatus, latency_bytes), 4);
  }
}
