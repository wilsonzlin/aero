# Test images (local-only)

This directory is intentionally **gitignored** (except this README).

It is used for large binaries that we cannot/should not commit:

- Open-source OS media downloaded by scripts (e.g. FreeDOS).
- User-supplied proprietary media (e.g. Windows 7).

## Prepare open-source images

```bash
bash ./scripts/prepare-freedos.sh
```

This writes `test-images/freedos/fd14-boot-aero.img` which is a FreeDOS 1.4 boot
floppy patched to print `AERO_FREEDOS_OK` to `COM1` during startup.

## Windows images (local only)

Windows images must be provided by the developer and **must not be committed**.
See:

```bash
bash ./scripts/prepare-windows7.sh
```

If you need to prepare a Windows 7 SP1 install ISO to load Aero drivers/certs during setup and first boot, see:

- `docs/16-windows7-install-media-prep.md`
