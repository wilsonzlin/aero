@echo off
rem This file is sourced by guest-tools\setup.cmd and guest-tools\uninstall.cmd.
rem Keep these values in sync with:
rem - emulator-presented PCI IDs
rem - the driver INFs (Hardware IDs / Compatible IDs)
rem - the storage driver's service name (INF AddService name)

rem ---------------------------
rem Boot-critical storage (virtio-blk)
rem ---------------------------

rem Service name for the virtio-blk storage miniport.
rem This MUST match the INF AddService name for the storage driver.
rem Default aligns with the upstream virtio-win `viostor` package.
set "AERO_VIRTIO_BLK_SERVICE=viostor"

rem Optional explicit .sys name for the storage driver.
rem If empty, setup.cmd assumes "<service>.sys".
set "AERO_VIRTIO_BLK_SYS="

rem Space-separated list of PCI hardware IDs (VEN/DEV pairs) for virtio-blk.
rem Include both "legacy/transitional" and "modern" virtio-pci IDs if applicable.
rem Note: IDs are individually quoted so the value can safely contain `&` characters.
set AERO_VIRTIO_BLK_HWIDS="PCI\VEN_1AF4&DEV_1001" "PCI\VEN_1AF4&DEV_1042"

rem ---------------------------
rem Non-boot-critical devices (used for documentation / potential future checks)
rem ---------------------------

rem Service name for the virtio-snd audio driver.
rem Used by verify.ps1 to confirm device binding.
rem Default aligns with upstream virtio-win (`viosnd`), but verify.ps1 also checks
rem common clean-room/Aero candidates (e.g. `virtiosnd`, `aeroviosnd`).
set "AERO_VIRTIO_SND_SERVICE=viosnd"

rem Optional explicit .sys name for the virtio-snd driver.
rem If empty, tools assume "<service>.sys".
set "AERO_VIRTIO_SND_SYS="

set AERO_VIRTIO_NET_HWIDS="PCI\VEN_1AF4&DEV_1000" "PCI\VEN_1AF4&DEV_1041"
set AERO_VIRTIO_INPUT_HWIDS="PCI\VEN_1AF4&DEV_1011" "PCI\VEN_1AF4&DEV_1052"
set AERO_VIRTIO_SND_HWIDS="PCI\VEN_1AF4&DEV_1059"

rem Aero WDDM GPU stack.
rem Must match emulator-presented IDs and the AeroGPU display driver INF.
rem The display driver supports both the new ABI (VEN_A3A0) and a legacy bring-up ABI (VEN_1AED).
rem Note: the older 1AE0-family vendor ID is stale/deprecated; keep it out of Guest Tools config.
set AERO_GPU_HWIDS="PCI\VEN_A3A0&DEV_0001" "PCI\VEN_1AED&DEV_0001"
