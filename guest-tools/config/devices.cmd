@echo off
rem -----------------------------------------------------------------------------
rem GENERATED FILE - DO NOT EDIT MANUALLY
rem
rem Source of truth: Windows device contract JSON
rem Generator: scripts/generate-guest-tools-devices-cmd.py
rem Contract name: aero-windows-pci-device-contract
rem Contract schema_version: 1
rem Contract version: 1.0.5
rem -----------------------------------------------------------------------------

rem This file is sourced by guest-tools\setup.cmd and guest-tools\uninstall.cmd.

rem ---------------------------
rem Boot-critical storage (virtio-blk)
rem ---------------------------

set "AERO_VIRTIO_BLK_SERVICE=aero_virtio_blk"
set "AERO_VIRTIO_BLK_SYS="
set AERO_VIRTIO_BLK_HWIDS="PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01" "PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4" "PCI\VEN_1AF4&DEV_1042&REV_01" "PCI\VEN_1AF4&DEV_1042"

rem ---------------------------
rem Non-boot-critical devices (used by verify.ps1)
rem ---------------------------

set "AERO_VIRTIO_NET_SERVICE=aero_virtio_net"
set AERO_VIRTIO_NET_HWIDS="PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01" "PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4" "PCI\VEN_1AF4&DEV_1041&REV_01" "PCI\VEN_1AF4&DEV_1041"
set "AERO_VIRTIO_INPUT_SERVICE=aero_virtio_input"
set AERO_VIRTIO_INPUT_HWIDS="PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4" "PCI\VEN_1AF4&DEV_1052&REV_01" "PCI\VEN_1AF4&DEV_1052"
set "AERO_VIRTIO_SND_SERVICE=aero_virtio_snd"
set "AERO_VIRTIO_SND_SYS="
set AERO_VIRTIO_SND_HWIDS="PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01" "PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4" "PCI\VEN_1AF4&DEV_1059&REV_01" "PCI\VEN_1AF4&DEV_1059"
rem
rem AeroGPU HWIDs:
rem   - PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0
rem   - PCI\VEN_A3A0&DEV_0001
rem Legacy AeroGPU device models are intentionally out of scope for Guest Tools; use drivers/aerogpu/packaging/win7/legacy with emulator/aerogpu-legacy if needed.
set "AERO_GPU_SERVICE=aerogpu"
set AERO_GPU_HWIDS="PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0" "PCI\VEN_A3A0&DEV_0001"

