# Third-party notices (Aero Guest Tools)

The Aero Guest Tools ISO/zip may include third-party software components.

In particular, when Guest Tools media is built from an upstream `virtio-win.iso`
(via `drivers/scripts/make-guest-tools-from-virtio-win.ps1`), it redistributes
Windows virtio driver packages from the **virtio-win** project (aka
`kvm-guest-drivers-windows`).

For compliant redistribution, ensure the Guest Tools media is shipped with the
upstream license texts and any attribution/notice files required by the
specific virtio-win artifacts being redistributed.

When Guest Tools is built via `make-guest-tools-from-virtio-win.ps1`, the build
process will also attempt to copy upstream virtio-win license/notice files (if
present) into the packaged media under:

- `licenses/virtio-win/`

See also:

- `docs/virtio-windows-drivers.md` (licensing/attribution policy and workflow)
- `drivers/virtio/THIRD_PARTY_NOTICES.md` (virtio driver notice template / source of truth)
