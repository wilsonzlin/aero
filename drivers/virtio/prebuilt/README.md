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

