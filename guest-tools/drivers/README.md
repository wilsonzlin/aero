# Driver payload

The Guest Tools scripts scan `guest-tools\drivers\<arch>\` recursively for `*.inf` driver packages and stage/install them via `pnputil`.

Populate these directories at release time:

- `guest-tools\drivers\x86\` for Windows 7 x86
- `guest-tools\drivers\amd64\` for Windows 7 x64

Typical layout is one folder per device (virtio-blk/net/snd/input, Aero GPU), but any structure is fine as long as the `*.inf` files are present.

Driver packages should also include any INF-referenced payload files alongside the INF (at minimum `.sys` + `.cat`, and optionally coinstallers/UMDs such as `*.dll`).
Canonical naming:

- Use `aerogpu` (not `aero-gpu`) for the AeroGPU driver directory name, matching the INF (`aerogpu.inf`) and source tree (`drivers/aerogpu/`).
