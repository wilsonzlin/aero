# `inf/` - driver package staging directory

This folder is intended to be the **exact directory you point Windows to** during installation:

- Device Manager → **Have Disk…** → select `virtio-input.inf`
- `pnputil -i -a virtio-input.inf` (Windows 7)

In a finished package, this directory will contain (at minimum):

```text
virtio-input.inf
virtio-input.cat
aero_virtio_input.sys
```

## Notes

### Catalog generation (`Inf2Cat`)

`Inf2Cat` hashes every file referenced by the INF. That means:

- `aero_virtio_input.sys` must exist in this folder before running `..\scripts\make-cat.cmd`.
- If you add extra payload files later (coinstallers, firmware blobs, etc), update the INF and regenerate the catalog.

### KMDF version / coinstaller

The INF declares a minimum **KMDF library version 1.9**, which is **in-box on Windows 7 SP1**.

If the driver is later built against a newer KMDF version, you will need to:

1. Update `KmdfLibraryVersion` in `virtio-input.inf`
2. Add the appropriate `WdfCoInstallerXXXX.dll` to the package and reference it in the INF
3. Regenerate the catalog and re-sign

### Release packaging

Once the driver binaries exist, `..\scripts\package-release.ps1` can be used to produce a redistributable zip that:

- Includes the INF (and CAT if present) from this directory
- Pulls the built SYS (and optional KMDF coinstaller DLL) from the `-InputDir` you provide
