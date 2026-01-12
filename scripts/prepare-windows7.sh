#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
Windows 7 test images are NOT provided by Aero (legal compliance).

To run the gated Windows 7 boot test locally:

  1) Place your own disk image at:
       test-images/local/windows7.img
     (or set AERO_WINDOWS7_IMAGE=/path/to/image)

     If you need to build/patch a Windows 7 SP1 install ISO to include Aero drivers,
     certificates, and boot policy flags (testsigning/nointegritychecks), see:
       docs/16-windows7-install-media-prep.md
     For Aero's canonical Windows 7 storage topology (AHCI HDD + IDE/ATAPI CD-ROM), see:
       docs/05-storage-topology-win7.md

  2) Optionally, provide a golden framebuffer snapshot:
       test-images/local/windows7_login.png
     (or set AERO_WINDOWS7_GOLDEN=/path/to/golden.png)

  3) Run the ignored test (recommended: use safe-run for timeouts/memory limits):
       AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p emulator --test windows7_boot --locked -- --ignored

 Notes:
      - Do NOT commit Windows images or golden screenshots.
      - Use a valid Windows 7 license and installation media.
EOF
