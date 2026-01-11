@echo off
rem This file is GENERATED from docs/windows-device-contract.json.
rem Do not edit by hand.

rem ---------------------------
rem Boot-critical storage (virtio-blk)
rem ---------------------------

set "AERO_VIRTIO_BLK_SERVICE=aerovblk"
set "AERO_VIRTIO_BLK_SYS="
set AERO_VIRTIO_BLK_HWIDS="PCI\VEN_1AF4&DEV_1042" "PCI\VEN_1AF4&DEV_1042&REV_01" "PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4" "PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01"

rem ---------------------------
rem Network / input / sound
rem ---------------------------

set "AERO_VIRTIO_NET_SERVICE=aerovnet"
set AERO_VIRTIO_NET_HWIDS="PCI\VEN_1AF4&DEV_1041" "PCI\VEN_1AF4&DEV_1041&REV_01" "PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4" "PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01"
set "AERO_VIRTIO_INPUT_SERVICE=aero_virtio_input"
set AERO_VIRTIO_INPUT_HWIDS="PCI\VEN_1AF4&DEV_1052" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4" "PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4"
set "AERO_VIRTIO_SND_SERVICE=aeroviosnd"
set AERO_VIRTIO_SND_HWIDS="PCI\VEN_1AF4&DEV_1059" "PCI\VEN_1AF4&DEV_1059&REV_01" "PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4" "PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01"

rem ---------------------------
rem Aero GPU
rem ---------------------------

set "AERO_GPU_SERVICE=aerogpu"
set AERO_GPU_HWIDS="PCI\VEN_A3A0&DEV_0001" "PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0"
