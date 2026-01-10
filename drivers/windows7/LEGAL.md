# Windows 7 virtio drivers: licensing & clean-room policy

This directory is intended for Aero’s Windows 7 **virtio** driver stack (storage/network/audio/input, plus any installer tooling).

**Goal:** keep everything in `drivers/windows7/` distributable as open source under Aero’s intended permissive licensing (**MIT OR Apache-2.0**) without accidentally importing code or text that is (a) not redistributable, or (b) copyleft (GPL/LGPL/AGPL).

This document is developer guidance for the Aero repository (it is **not legal advice**).

---

## 1) Candidate upstream sources (WDK/SDK samples) and compatibility review

Windows driver development often starts from Microsoft samples. The names below are the *most likely* samples a Windows 7 virtio driver author will reach for:

- **Sysvad** (audio)
- **vhidmini** (HID)
- **StorPort** miniport samples (storage)
- **NDIS 6.x** miniport samples (network)

The same sample may exist in multiple “upstreams” (old WDK installs vs the modern open-source sample repo). **Their licensing is not necessarily the same.**

### How to read the results below

For each sample, we list:

1. **Common upstream locations** (typical WDK paths and the public “Windows driver samples” repo).
2. **What the source headers usually say** (what you’ll see at the top of many `.c/.h` files).
3. **Redistribution terms** (where the permission actually comes from).
4. **Verdict for Aero** (can we copy into this repo under MIT/Apache?).

#### A note on “All rights reserved” headers

Many Microsoft driver sample files begin with text like the following (example excerpt):

```text
Copyright (c) Microsoft Corporation.  All rights reserved.

THIS CODE AND INFORMATION IS PROVIDED "AS IS" WITHOUT WARRANTY OF
ANY KIND, EITHER EXPRESSED OR IMPLIED, INCLUDING BUT NOT LIMITED TO
THE IMPLIED WARRANTIES OF MERCHANTABILITY AND/OR FITNESS FOR A
PARTICULAR PURPOSE.
```

That header is a warranty disclaimer and a copyright notice. **It is not, by itself, a permission to copy/redistribute/relicense**. Any redistribution rights would have to come from an accompanying license file/EULA.

#### A note on the open-source Microsoft driver samples repo

Microsoft’s public **Windows driver samples** repository (commonly `microsoft/Windows-driver-samples`) is distributed under the **MIT License** (check the repo’s `LICENSE` file in the specific revision you are using).

MIT redistribution terms (summary): you may copy/modify/redistribute, including in proprietary products, **as long as you keep the copyright notice and the MIT permission notice**.

MIT license text (key grant + condition, excerpt):

```text
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.
```

---

### 1.1 Sysvad (audio driver sample)

**What it is:** a sample audio driver often used to learn PortCls/WDM/KMDF audio driver structure, topology, and property handling.

**Typical upstream locations**

- Windows 7-era WDK installs: `...\src\audio\sysvad\...`
  - Example (WDK 7.1 default-ish layout): `C:\WinDDK\7600.16385.1\src\audio\sysvad\...`
- Open-source Microsoft samples repo: `audio/sysvad/`

**License header text you’ll commonly see**

- Frequently includes the “Microsoft Corporation. All rights reserved.” + “AS IS” disclaimer excerpt shown above.

**Redistribution terms**

- **From a WDK/SDK installation**: redistribution is governed by the **WDK/SDK license terms** that shipped with that kit. Those terms are not generally written as an OSI open-source license and **are not automatically compatible with relicensing under MIT OR Apache-2.0**.
- **From the public Microsoft samples repo**: redistribution is governed by the repo’s **MIT License**.

**Verdict for Aero**

- ✅ **OK to copy** only if the Sysvad code you’re using is under an OSI-permissive license (e.g., MIT from the public samples repo) and you preserve required notices.
- ❌ **Do not copy** Sysvad source/text directly from a WDK/SDK install unless its license terms explicitly allow open redistribution of source under permissive terms compatible with Aero.

---

### 1.2 vhidmini (virtual HID mini driver sample)

**What it is:** a sample virtual HID mini driver commonly used to understand HID report descriptors, IOCTL handling, PnP/Power, and INF packaging.

**Typical upstream locations**

- Windows 7-era WDK installs: `...\src\hid\vhidmini\...`
  - Example (WDK 7.1 default-ish layout): `C:\WinDDK\7600.16385.1\src\hid\vhidmini\...`
- Open-source Microsoft samples repo: `hid/vhidmini/`

**License header text you’ll commonly see**

- Frequently includes the “All rights reserved” + “AS IS” disclaimer excerpt shown above.

**Redistribution terms**

- **WDK/SDK installation copy**: governed by that kit’s license terms (not an OSI open-source license by default).
- **Public samples repo copy**: governed by the repo’s MIT License.

**Verdict for Aero**

- ✅ **OK to copy** only from an OSI-permissive upstream (e.g., MIT-licensed public samples repo) with proper attribution.
- ❌ **Do not copy** from the in-kit sample without an explicit open redistribution grant compatible with Aero.

---

### 1.3 StorPort miniport samples (storage)

**What it is:** examples of storage miniports (StorPort / SCSI / related) that are a common starting point for a virtio-blk / virtio-scsi style Windows 7 storage driver.

**Typical upstream locations**

- Windows 7-era WDK installs: `...\src\storage\...` (often under `miniports`, `storport`, or similar subtrees depending on kit)
  - Example (WDK 7.1 default-ish layout): `C:\WinDDK\7600.16385.1\src\storage\...`
- Open-source Microsoft samples repo: typically under `storage/` (exact path varies by sample)

**License header text you’ll commonly see**

- Frequently includes the “All rights reserved” + “AS IS” disclaimer excerpt shown above.

**Redistribution terms**

- **WDK/SDK installation copy**: governed by that kit’s license terms.
- **Public samples repo copy**: governed by MIT License.

**Verdict for Aero**

- ✅ **OK to copy** only from a clearly permissive upstream (MIT, Apache-2.0, BSD, etc.) with preserved notices.
- ❌ **Do not copy** StorPort sample source from WDK/SDK installs into Aero unless the kit’s license explicitly permits open redistribution of source under terms compatible with Aero.

---

### 1.4 NDIS 6.x miniport samples (network)

**What it is:** NDIS miniport driver samples used as a starting point for Windows 7 network drivers (e.g., for virtio-net). Common sample names historically include things like “netvmini” or “passthru” (exact names vary by kit).

**Typical upstream locations**

- Windows 7-era WDK installs: `...\src\network\ndis\...`
  - Example (WDK 7.1 default-ish layout): `C:\WinDDK\7600.16385.1\src\network\ndis\...`
- Open-source Microsoft samples repo: typically under `network/ndis/`

**License header text you’ll commonly see**

- Frequently includes the “All rights reserved” + “AS IS” disclaimer excerpt shown above.

**Redistribution terms**

- **WDK/SDK installation copy**: governed by that kit’s license terms.
- **Public samples repo copy**: governed by MIT License.

**Verdict for Aero**

- ✅ **OK to copy** only from a clearly permissive upstream (MIT, Apache-2.0, BSD, etc.) with preserved notices.
- ❌ **Do not copy** NDIS sample source from WDK/SDK installs into Aero unless the kit’s license explicitly permits open redistribution of source under terms compatible with Aero.

---

## 2) Policy for Aero Windows 7 driver developers (normative)

### 2.1 Default rule: Aero driver code is clean-room, permissively licensed

- New code added to `drivers/windows7/` **MUST** be authored for Aero and intended to be distributed under **MIT OR Apache-2.0**.
- Contributors **MUST NOT** paste in “convenient” chunks from unknown origins (including old WDK samples, driver blogs, StackOverflow gists, etc.) without verifying license.

### 2.2 Copying from Microsoft samples: allowed only with an OSI-permissive license + attribution

Direct copying from Microsoft samples is **allowed** only if **all** of the following are true:

1. The specific upstream sample revision is under an **OSI-permissive license** suitable for inclusion in Aero (preferably MIT; Apache-2.0/BSD/ISC also acceptable).
2. You can point to the governing license file (e.g., `LICENSE`) and record the upstream URL + revision.
3. You preserve required notices (copyright + license text).

If any of the above are not true, the sample is **reference only** (see below).

**Practical guidance**

- Prefer the public Microsoft `Windows-driver-samples` repository when you need sample scaffolding, because it is MIT licensed.
- Treat samples found only inside a local WDK/SDK install as **not safe to copy** unless their license terms explicitly grant open redistribution rights compatible with Aero.

### 2.3 Reference-only usage (clean-room implementation)

When a sample is “reference only”:

- You may use it to understand **APIs, expected call order, and structure**.
- You **MUST NOT** copy:
  - code (including small helpers/macros),
  - comments,
  - INF text blocks,
  - tables/arrays (e.g., report descriptors) unless independently derived from a spec you can cite.

Instead:

1. Write a short design note in the Aero codebase describing the behavior using *your own words*, with citations to public docs/specs.
2. Implement from that design note + public specifications.

### 2.4 Forbidden sources (GPL/copy-left contamination avoidance)

To keep Aero’s Windows 7 drivers permissively licensed and redistributable, the following are **forbidden sources** for copying code or “translating” code line-by-line:

- **QEMU** source code (GPL)
- **virtio-win** source code (license mix; explicitly avoid any GPL portions and avoid the repo entirely as a safe default)
- **Linux kernel** virtio drivers (GPL)
- Any other **GPL/LGPL/AGPL**-licensed implementation

If you need virtio behavior details, use the **virtio specification** and public documentation, not other implementations’ source.

### 2.5 Allowed references (safe sources)

The following are generally safe to use as references when writing clean-room code:

- **Virtio specifications** (device and PCI transport)
- **PCI / PCIe** specifications
- Microsoft’s **public documentation** (WDK docs, MSDN, protocol docs)
- Publicly available protocol specs (USB HID spec, etc.)
- Black-box observations (logs/ETW traces/packet captures) of Windows behavior

### 2.6 Source tracking requirements

For each driver (or major subsystem) under `drivers/windows7/`, add a short `SOURCES.md` (or a section in that driver’s `README.md`) that lists:

- the specs used (virtio version, PCI references, HID spec, etc.)
- any sample repos consulted (even if reference-only)
- any code copied from permissively licensed sources (with upstream URL + revision)

This makes later audits fast and prevents “where did this come from?” license archaeology.

---

## 3) Contributor checklist (before submitting Windows 7 driver code)

- [ ] Every new source file includes an SPDX header for Aero’s license intent (e.g., `SPDX-License-Identifier: MIT OR Apache-2.0`) unless it is third-party code under a different permissive license.
- [ ] I did **not** copy code/comments/INF blocks from a WDK/SDK install sample whose license terms are unclear or non-OSI.
- [ ] If I copied from a permissively licensed upstream (e.g., MIT Microsoft samples repo), I recorded:
  - [ ] upstream URL and revision
  - [ ] original license text / notice requirements
  - [ ] where the copied code lives in Aero
- [ ] I did **not** use QEMU, virtio-win, or Linux kernel code as a source for implementation (copying or “translating”).
- [ ] I can explain every “magic value” (feature bit, config field, descriptor layout) with a citation to a spec or public documentation.
