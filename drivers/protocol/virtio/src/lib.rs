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
  pub num_buffers: u16,
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
    assert_eq!(size_of::<VirtioNetHdr>(), 12);
    assert_eq!(offset_of!(VirtioNetHdr, flags), 0);
    assert_eq!(offset_of!(VirtioNetHdr, hdr_len), 2);
    assert_eq!(offset_of!(VirtioNetHdr, num_buffers), 10);
  }
}

