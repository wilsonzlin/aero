# Place real driver binaries here

Populate this directory with a Windows 7-capable virtio driver set (typically from `virtio-win.iso`):

```
drivers/virtio/prebuilt/
  win7/
    x86/
      viostor/viostor.inf + .sys + .cat
      netkvm/netkvm.inf + .sys + .cat
      (optional) vioinput/...
      (optional) viosnd/...
    amd64/
      ...
```

Then build the drivers ISO:

```bash
python3 tools/driver-iso/build.py \
  --drivers-root drivers/virtio/prebuilt \
  --output dist/aero-virtio-win7-drivers.iso
```

Redistribution note:

- Ensure you ship `THIRD_PARTY_NOTICES.md` (see `drivers/virtio/THIRD_PARTY_NOTICES.md`) alongside any virtio-win-derived binaries.
- If you are redistributing drivers extracted from a virtio-win ISO, also include upstream license/notice texts under `licenses/virtio-win/` when available.

Tip: the script `drivers/scripts/make-driver-pack.ps1` already produces a compatible staging directory
at `drivers/out/aero-win7-driver-pack/`. You can point the ISO builder at that directory directly
instead of copying into `drivers/virtio/prebuilt/`.

Note: driver directories may also contain additional payload files referenced by the INF, such as
KMDF coinstallers (`WdfCoInstaller*.dll`) or other helper DLLs.
