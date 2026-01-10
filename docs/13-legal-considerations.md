# 13 - Legal & Licensing Considerations

## Overview

Building a Windows 7 emulator involves significant legal considerations around intellectual property, software licensing, and distribution.

This document provides background and rationale. For the **repository’s
authoritative policies and templates**, see:

- [`../LEGAL.md`](../LEGAL.md)
- [`../CONTRIBUTING.md`](../CONTRIBUTING.md)
- [`../TRADEMARKS.md`](../TRADEMARKS.md)
- [`../DMCA_POLICY.md`](../DMCA_POLICY.md)
- [`../SECURITY.md`](../SECURITY.md)
- [`./LICENSE.md`](./LICENSE.md) (documentation licensing)

---

## Key Legal Areas

### 1. Emulator Legality

**General Principle:** Emulation itself is legal. Key precedents:

- **Sony v. Connectix (2000):** Ruled that reverse engineering for compatibility is fair use
- **Sega v. Accolade (1992):** Established that reverse engineering for interoperability is protected

**What We Can Do:**
- ✅ Build an emulator that runs x86/x64 code
- ✅ Implement hardware interfaces based on public specifications
- ✅ Reverse engineer undocumented behavior for compatibility

**What We Cannot Do:**
- ❌ Distribute Microsoft copyrighted code (Windows, BIOS, drivers)
- ❌ Use Microsoft trademarks without permission
- ❌ Circumvent DRM or copy protection

---

### 2. Windows 7 Licensing

#### End User License Agreement (EULA)

Windows 7 retail/OEM licenses permit:
- Installation on one physical computer
- Use with virtualization software on the licensed machine

**Key Considerations:**

1. **Users Must Supply Their Own License**
   - Aero does not include Windows 7
   - Users must have a valid Windows 7 license
   - License key validation happens within Windows itself

2. **Volume Licensing**
   - Enterprise customers may have VL agreements
   - Some VL agreements allow virtual instances
   - Users responsible for compliance

3. **Extended Security Updates (ESU)**
   - Windows 7 reached end of support January 2020
   - ESU available for enterprise through 2023
   - No longer receiving security updates

#### Distribution Model

```
┌─────────────────────────────────────────────────────────────────┐
│                    Legal Distribution Model                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Aero Project Provides:                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  • Emulator software (open source)                       │    │
│  │  • Custom BIOS (open source)                             │    │
│  │  • Documentation                                         │    │
│  │  • Virtio drivers (open source)                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  User Must Provide:                                              │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  • Windows 7 installation media (ISO)                    │    │
│  │  • Valid Windows 7 license key                           │    │
│  │  • Acceptance of Microsoft EULA                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

### 3. BIOS/Firmware

#### Traditional BIOS

- IBM PC BIOS is copyrighted
- We **cannot** use or distribute original BIOS
- We **must** use open-source alternatives

#### Our Approach: Custom BIOS

```rust
// Aero uses a custom, open-source BIOS implementation
// Licensed under MIT/Apache-2.0 dual license

pub struct AeroBios {
    // No copyrighted code
    // Implements required functionality from scratch
    // Based on public specifications
}
```

**Open-Source BIOS Options:**
- SeaBIOS (LGPL v3)
- coreboot (GPL v2)
- Custom implementation (MIT/Apache-2.0)

**Recommendation:** Custom implementation to avoid GPL complications with WASM

---

### 4. Device Specifications

#### Public Specifications (Safe to Use)

| Component | Specification Source | License |
|-----------|---------------------|---------|
| x86/x64 CPU | Intel/AMD SDMs | Public documentation |
| VGA/SVGA | VESA specifications | Public standard |
| PS/2 | Public documentation | Public standard |
| PCI/PCIe | PCI-SIG specifications | Membership (specs public) |
| USB | USB-IF specifications | Public standard |
| AHCI/SATA | Intel/SATA-IO | Public documentation |
| HD Audio | Intel specification | Public documentation |
| ACPI | UEFI Forum | Public specification |

#### Potentially Problematic

| Component | Issue | Mitigation |
|-----------|-------|------------|
| DirectX | Proprietary API | Implement from public docs, no MS code |
| WDDM | Windows-specific | Implement custom GPU driver |
| NTFS | Patented | Don't implement in emulator (guest OS handles) |

---

### 5. Patent Considerations

#### x86 Patents

- Most fundamental x86 patents have expired
- AMD64 (x86-64) patents still active but cross-licensed
- Emulation for personal use generally not challenged

#### Video Codec Patents

If implementing video playback:
- H.264/AVC: Patent pool (licensing required for commercial)
- VP8/VP9: Royalty-free (Google)
- AV1: Royalty-free (Alliance for Open Media)

**Recommendation:** Use browser's native codec support via WebCodecs

#### Software Patents in Emulation

- No known blocking patents for x86 emulation
- JIT compilation techniques are well-established
- Memory virtualization techniques are public domain

---

### 6. Open Source Licensing

#### Aero License Selection

**Recommended: MIT OR Apache-2.0 dual license**

```
SPDX-License-Identifier: MIT OR Apache-2.0

Copyright (c) 2026 Aero Contributors

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software...
```

See [`../LICENSE-MIT`](../LICENSE-MIT), [`../LICENSE-APACHE`](../LICENSE-APACHE),
and [`../NOTICE`](../NOTICE).

**Rationale:**
- Permissive for commercial use
- Compatible with most other licenses
- No patent retaliation clauses (Apache-2.0 provides patent grant)
- No viral/copyleft requirements

#### Dependency Licenses

| Dependency | License | Compatible? |
|------------|---------|-------------|
| Rust stdlib | MIT/Apache-2.0 | ✅ Yes |
| wasm-bindgen | MIT/Apache-2.0 | ✅ Yes |
| WebGPU | W3C | ✅ Yes |
| SeaBIOS | LGPL v3 | ⚠️ Requires care |
| QEMU (reference) | GPL v2 | ❌ Cannot copy code |

---

### 7. Trademark Considerations

#### What We Cannot Use

- "Windows" trademark
- Microsoft logo
- Windows 7 product imagery
- "Designed for Windows" logos

#### What We Can Say

- "Compatible with Windows 7"
- "Runs Windows 7" (factual statement)
- "x86 PC emulator"

#### Project Naming

- ✅ "Aero" - Generic term, not trademarked in this context
- ❌ "Windows Emulator" - Implies Microsoft affiliation
- ❌ "Win7Emu" - Uses "Win" which could cause confusion

See [`../TRADEMARKS.md`](../TRADEMARKS.md) for repo-wide naming/branding
guidelines and suggested disclaimers.

---

### 8. Distribution Considerations

#### Source Code Distribution

```
Aero Repository Structure:
├── src/              # MIT/Apache-2.0
├── bios/             # MIT/Apache-2.0 (custom)
├── drivers/          # MIT/Apache-2.0 (virtio)
├── docs/             # CC-BY-4.0
└── tests/            # MIT/Apache-2.0
```

#### Binary Distribution

- Pre-built WASM binaries: ✅ OK
- Including Windows files: ❌ NOT OK
- Including BIOS ROMs (not ours): ❌ NOT OK

#### Website/Service

If hosting Aero as a service:
- Do not provide Windows images
- Require users to upload their own
- Clear ToS requiring valid licenses
- No piracy facilitation

---

### 9. DMCA Considerations

#### Safe Harbor

As a platform provider, maintain DMCA safe harbor:
- Register DMCA agent
- Implement takedown procedures
- Don't have actual knowledge of infringement

See [`../DMCA_POLICY.md`](../DMCA_POLICY.md) for a takedown/counter-notice
template and repeat-infringer posture.

#### Circumvention Concerns

The DMCA prohibits circumventing "technological protection measures":
- Windows activation: Don't bypass (let Windows handle it)
- Game DRM: Don't specifically circumvent
- Region locks: User responsibility

---

### 10. International Considerations

#### EU

- Generally permissive of interoperability
- Computer Programs Directive allows reverse engineering
- GDPR compliance if collecting user data

#### Other Jurisdictions

- Japan: Generally permissive of emulation
- Australia: Fair dealing provisions apply
- Check local laws for specific markets

---

## Compliance Checklist

### Before Launch

- [ ] Legal review of codebase (no copyrighted code)
- [ ] Trademark clearance for project name
- [ ] License headers on all source files
- [ ] NOTICE file with attribution
- [ ] Terms of Service drafted
- [ ] Privacy Policy (if collecting data)
- [ ] DMCA agent registered

### Ongoing

- [ ] Monitor for legal challenges
- [ ] Respond to takedown requests
- [ ] Update licenses as dependencies change
- [ ] Review user-contributed code

---

## Disclaimers

### Required Disclaimers

```
Aero is an independent project and is not affiliated with, endorsed by,
or sponsored by Microsoft Corporation. Windows is a registered trademark
of Microsoft Corporation.

Users are responsible for ensuring they have valid licenses for any
software they run within Aero. The Aero project does not provide,
distribute, or facilitate access to Microsoft Windows or any other
copyrighted software.

Aero is provided "AS IS" without warranty of any kind.
```

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Microsoft takedown | Low | High | Clean-room implementation, no MS code |
| Patent claim | Very Low | High | Use established techniques |
| Trademark complaint | Low | Medium | Clear disclaimers, proper naming |
| DMCA abuse | Medium | Low | Proper safe harbor procedures |
| License violation | Low | Medium | Regular audits, clear policies |

---

## Recommendations

1. **Clean-Room Implementation**
   - Never copy or redistribute Microsoft proprietary source code (Windows, BIOS, etc.)
   - Microsoft driver *samples* may be usable if (and only if) they are under an OSI-permissive license (e.g., the MIT-licensed `Windows-driver-samples` repository) and required notices are preserved
   - See `drivers/windows7/LEGAL.md` for the Windows 7 virtio driver sourcing policy
   - Document all specifications used
   - Keep development logs

2. **Clear Boundaries**
   - Users provide their own Windows
   - No piracy facilitation
   - Educational/preservation focus

3. **Legal Consultation**
   - Consult IP attorney before major releases
   - Have legal review distribution model
   - Budget for potential legal defense

---

## Next Steps

- See [Project Milestones](./14-project-milestones.md) for development timeline
- See [Task Breakdown](./15-agent-task-breakdown.md) for implementation tasks
