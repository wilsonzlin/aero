# Aero Guest Tools (Windows 7)

`guest-tools/` contains the **in-guest, offline** installer experience for Aero's Windows drivers ("Aero Guest Tools").

It is designed for the default workflow:

1. Install Windows 7 using safe/legacy emulated devices (AHCI, e1000, HDA, PS/2, VGA).
2. Run `guest-tools/setup.cmd` inside the guest.
3. Power off / reboot and switch the VM devices to Aero virtio devices (virtio-blk/net/snd/input + Aero WDDM GPU).
4. Boot and let Plug and Play bind the newly-present devices to the staged driver packages.

See `guest-tools/README.md` for full instructions, troubleshooting, and recovery steps.

