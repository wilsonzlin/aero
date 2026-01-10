#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
Windows 7 test images are NOT provided by Aero (legal compliance).

To run the gated Windows 7 boot test locally:

  1) Place your own disk image at:
       test-images/local/windows7.img
     (or set AERO_WINDOWS7_IMAGE=/path/to/image)

  2) Optionally, provide a golden framebuffer snapshot:
       test-images/local/windows7_login.png
     (or set AERO_WINDOWS7_GOLDEN=/path/to/golden.png)

  3) Run the ignored test:
       cargo test --test windows7_boot -- --ignored

Notes:
  - Do NOT commit Windows images or golden screenshots.
  - Use a valid Windows 7 license and installation media.
EOF

