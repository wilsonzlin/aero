@echo off
rem -----------------------------------------------------------------------------
rem GENERATED FILE - DO NOT EDIT MANUALLY
rem
rem Source of truth: docs/windows-device-contract.json
rem Generator: scripts/generate-guest-tools-devices-cmd.py
rem Contract version: 1.0.2
rem -----------------------------------------------------------------------------

rem This file is sourced by guest-tools\setup.cmd and guest-tools\uninstall.cmd.

rem ---------------------------
rem Boot-critical storage (virtio-blk)
rem ---------------------------

set "AERO_VIRTIO_BLK_SERVICE=aerovblk"
set "AERO_VIRTIO_BLK_SYS="
set AERO_VIRTIO_BLK_HWIDS="PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01" "PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4" "PCI\VEN_1AF4&DEV_1042&REV_01" "PCI\VEN_1AF4&DEV_1042"

rem ---------------------------
rem Non-boot-critical devices (used by verify.ps1)
rem ---------------------------

set "AERO_VIRTIO_NET_SERVICE=aerovnet"
set AERO_VIRTIO_NET_HWIDS="PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01" "PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4" "PCI\VEN_1AF4&DEV_1041&REV_01" "PCI\VEN_1AF4&DEV_1041"
set "AERO_VIRTIO_INPUT_SERVICE=aero_virtio_input"
set AERO_VIRTIO_INPUT_HWIDS="PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4" "PCI\VEN_1AF4&DEV_1052&REV_01" "PCI\VEN_1AF4&DEV_1052"
set "AERO_VIRTIO_SND_SERVICE=aeroviosnd"
set "AERO_VIRTIO_SND_SYS="
set AERO_VIRTIO_SND_HWIDS="PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01" "PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4" "PCI\VEN_1AF4&DEV_1059&REV_01" "PCI\VEN_1AF4&DEV_1059"
set "AERO_GPU_SERVICE=aerogpu"
set AERO_GPU_HWIDS="PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0" "PCI\VEN_A3A0&DEV_0001"

