# Aero Guest Tools (Windows 7)

`guest-tools/` contains the **in-guest, offline** installer experience for Aero's Windows drivers ("Aero Guest Tools").

It is designed for the default workflow:

1. Install Windows 7 using safe/legacy emulated devices (AHCI HDD + IDE/ATAPI CD-ROM, e1000, HDA, PS/2, VGA).
   - See [`docs/05-storage-topology-win7.md`](./05-storage-topology-win7.md) for the canonical Win7 install/boot storage topology.
2. Mount the Guest Tools media and run `setup.cmd` inside the guest.
3. Power off / reboot and switch the VM devices to Aero virtio devices:
   - virtio-blk (storage)
   - virtio-net (network)
   - virtio-input (keyboard/mouse) *(optional)*
   - virtio-snd (audio) *(optional; browser runtime prefers HDA when present, so virtio-snd is active in HDA-less builds or with an explicit selection mechanism — keep HDA as fallback)*
   - Aero WDDM GPU
4. Boot and let Plug and Play bind the newly-present devices to the staged driver packages.

End-user guides:

- [`windows7-guest-tools.md`](./windows7-guest-tools.md)
- [`windows7-driver-troubleshooting.md`](./windows7-driver-troubleshooting.md)

See `guest-tools/README.md` for the authoritative ISO contents, script flags, and implementation details.
