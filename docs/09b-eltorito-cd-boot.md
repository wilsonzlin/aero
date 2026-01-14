# 09b - BIOS CD-ROM Boot (El Torito + INT 13h extensions)

This document captures the **minimal El Torito CD-ROM boot behavior** (plus the **INT 13h
extensions** surface) that Aero relies on to boot **Windows 7 install media**. It is intended to be
precise enough that a future contributor could re-implement the logic from scratch without
regressing Win7.

Scope:

* **El Torito, no-emulation boot only** (what Windows 7 install ISOs use for BIOS boot).
* **ISO9660 Volume Descriptor** scanning to find the El Torito boot catalog.
* **INT 13h extensions** used by Windows-style bootloaders: `AH=41h`, `AH=42h`, `AH=48h`.
* Explicit non-goal: floppy/hard-disk emulation modes, multi-entry boot menus, UEFI boot.

---

## Sector sizes and addressing (2048 vs 512)

El Torito and ISO9660 describe locations in **ISO logical blocks** of **2048 bytes**.

In Aero’s BIOS:

* **Externally (INT 13h for CD drives):** CD-ROM media is exposed as **2048-byte sectors** (and
  `AH=48h` reports `bytes_per_sector = 2048`).
* **Internally:** there are two relevant backends:
  * **El Torito boot/catalog scanning** uses the firmware `BlockDevice` interface (**512-byte
    sectors**) and converts ISO LBAs (`2048-byte sectors`) into 512-byte LBAs.
  * **INT 13h CD-ROM reads** may be backed either by a firmware `CdromDevice` (**2048-byte sectors**)
    or (legacy fallback) by exposing the raw ISO bytes via `BlockDevice` (**512-byte sectors**) and
    letting the BIOS perform the conversion.

When using the 512-byte `BlockDevice` path for ISO media, this creates a critical conversion rule
between **2048-byte LBAs** and the underlying 512-byte sector backend:

* `lba512 = lba2048 * 4` (and similarly `count512 = count2048 * 4`)

You will see this conversion repeatedly:

* ISO9660 volume descriptors start at **ISO LBA 16** → underlying 512-sector LBA `16 * 4 = 64`.
* The Boot Record volume descriptor’s **boot catalog pointer** is an **ISO LBA**.
* The selected boot entry’s **`load_rba`** is an **ISO LBA**.
* The selected boot entry’s **`sector_count`** is in **512-byte sectors** (already the
  underlying unit), even though the catalog’s LBAs are 2048-byte ISO LBAs.

If you forget which fields are 2048-LBA vs 512-sector units, you will load the wrong bytes and
Windows boot will fail very early.

---

## 1) ISO9660 Volume Descriptor scan (starts at ISO LBA 16)

ISO9660 volume descriptors are a contiguous sequence of 2048-byte sectors starting at **ISO LBA 16**
(`0x10`). Aero’s CD boot logic scans them linearly until it finds the **Boot Record Volume
Descriptor** that advertises the El Torito boot catalog.

### Scan algorithm (conceptual)

1. For `iso_lba = 16..`:
    1. Read 2048 bytes from the CD at `iso_lba`.
       * If your underlying disk interface is 512-byte sectors, this means reading 4 consecutive
         512-byte sectors at `lba512 = iso_lba * 4`.
    2. Validate `vd[1..6] == "CD001"` (standard identifier) and `vd[6] == 0x01` (version).
    3. Dispatch on `vd[0]` (volume descriptor type):
       * `0x00` → **Boot Record VD** (El Torito is identified by a magic string; see below)
       * `0x01` → Primary Volume Descriptor (not used for El Torito discovery)
       * `0x02` → Supplementary (Joliet) (not used for El Torito discovery)
       * `0xFF` → Volume Descriptor Set Terminator (**stop scanning**)
       * other types are ignored
2. If we hit the terminator without finding a valid El Torito Boot Record VD, the ISO is treated as
   **not BIOS-bootable via El Torito**.

Implementation note: Aero scans until it sees the Volume Descriptor Set Terminator (`0xFF`) or
encounters a non-ISO9660 descriptor; there is no separate fixed scan cap beyond the image boundary.

---

## 2) Boot Record Volume Descriptor (El Torito pointer)

The El Torito boot catalog is located via a special ISO9660 Volume Descriptor: the **Boot Record
Volume Descriptor**.

Required fields:

* `type` = `0x00`
* `standard_id` = `"CD001"`
* `version` = `0x01`
* `boot_system_id` = `"EL TORITO SPECIFICATION"` (ASCII, space-padded)
* `boot_catalog_lba` at offset `0x47` (little-endian `u32`, **ISO 2048-byte LBA**)

### Boot Record VD layout (2048 bytes)

| Offset | Size | Meaning | Notes |
|---:|---:|---|---|
| `0x00` | 1 | Volume Descriptor Type | Must be `0x00` for Boot Record |
| `0x01` | 5 | Standard Identifier | Must be ASCII `"CD001"` |
| `0x06` | 1 | Version | Usually `0x01` |
| `0x07` | 32 | Boot System Identifier | Must equal `"EL TORITO SPECIFICATION"` (padded) |
| `0x27` | 32 | Boot System Use | Unused by Aero |
| `0x47` | 4 | Boot Catalog Pointer | **ISO LBA (2048-byte)**, little-endian `u32` |
| `0x4B` | … | Reserved | Unused |

Once `boot_catalog_lba` is read, convert to BIOS sectors if needed:

* `boot_catalog_lba512 = boot_catalog_lba2048 * 4`

---

## 3) Boot Catalog (validation entry + boot entry scanning)

The **Boot Catalog** is an array of 32-byte entries (typically stored in 2048-byte ISO blocks).
The first catalog block normally begins with a **Validation Entry** at offset `0x00` (entry #0),
followed by the “initial/default entry” at offset `0x20` (entry #1).

In the Windows install ISO case, the bootable BIOS no-emulation entry is commonly the
initial/default entry, but Aero does **not** assume that; it scans entries for the first usable BIOS
no-emulation boot entry.

Aero’s implemented behavior:

1. Read the catalog starting at `boot_catalog_lba` (2048-byte ISO LBA).
   * We read a bounded prefix (currently **up to 4 ISO blocks**) for safety.
2. Parse entry #0 (offset `0x00`) as the **Validation Entry** and validate it (checksum + key bytes).
3. Scan subsequent 32-byte entries for the first **bootable, no-emulation, BIOS/x86** boot entry:
   * Bootable: `boot_indicator == 0x88`
   * No-emulation: `boot_media_type == 0x00`
   * Platform: **x86 BIOS** (platform id `0`)
   * Section header entries (`0x90`/`0x91`) update the current platform id for subsequent entries.
   * Unknown/extension entries are ignored.

Entries are 32 bytes each.

### 3.1 Validation Entry (required)

The Validation Entry is a fixed-format 32-byte record that validates the catalog itself.

| Offset | Size | Field | Required value / rule |
|---:|---:|---|---|
| `0x00` | 1 | Header ID | Must be `0x01` |
| `0x01` | 1 | Platform ID | Common values: `0x00` x86 BIOS, `0xEF` EFI. This is the default platform for the initial/default entry; section headers can override it. Aero only boots BIOS/x86 entries. |
| `0x02` | 2 | Reserved | Typically `0x0000` |
| `0x04` | 24 | ID string | Ignored (space padded) |
| `0x1C` | 2 | Checksum | See checksum rule below |
| `0x1E` | 1 | Key byte 1 | Must be `0x55` |
| `0x1F` | 1 | Key byte 2 | Must be `0xAA` |

#### Validation checksum rule

Interpret the 32-byte validation entry as **16 little-endian `u16` words**. The catalog is valid if:

```
sum(words[0..16]) mod 0x10000 == 0
```

Pseudo-code:

```text
sum = 0u16
for i in [0, 2, 4, ..., 30]:
  sum = sum + u16_le(entry[i..i+2])
valid = (sum == 0)
```

If the checksum or the key bytes are wrong, **do not attempt to boot**.

### 3.2 Boot Entry (initial/default entry on Windows media)

The boot entry specifies the boot image location and how to load it. On Windows install media this
is typically the “Initial/Default Entry”.

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| `0x00` | 1 | Boot Indicator | `0x88` = bootable, `0x00` = not bootable |
| `0x01` | 1 | Boot Media Type | `0x00` = **no emulation** (required for Win7) |
| `0x02` | 2 | Load Segment | `u16` LE. If `0`, BIOS must default to **`0x07C0`** |
| `0x04` | 1 | System Type | Ignored by Aero for no-emulation |
| `0x05` | 1 | Unused | Must be ignored |
| `0x06` | 2 | Sector Count | `u16` LE, **count of 512-byte sectors to load**. If `0`, BIOS must default to **4×512B sectors** (**2048 bytes**) for **no-emulation** boot (per spec; Windows install media relies on this). |
| `0x08` | 4 | Load RBA | `u32` LE, **ISO LBA (2048-byte)** of boot image |
| `0x0C` | 20 | Unused | Must be ignored |

Minimal acceptance rules:

* Bootable (`boot_indicator == 0x88`)
* No-emulation (`boot_media_type == 0x00`)

Other media types (floppy/hdd emulation) are intentionally unsupported in the minimal path. Aero
does **not** implement interactive “boot menus”; if multiple bootable BIOS no-emulation entries
exist, Aero will simply pick the **first** one it finds during the scan described above.

---

## 4) No-emulation boot image loading rules (Win7 critical)

Given the selected boot entry:

1. Compute `load_segment`:
   * If `load_segment != 0`, use it.
   * If `load_segment == 0`, default to **`0x07C0`**.
2. Compute `sector_count`:
   * If `sector_count != 0`, use it.
   * If `sector_count == 0`, default to **4** (2048 bytes total).
3. Compute destination physical address: `dst = load_segment << 4`.
4. Compute how many bytes to read: `bytes_to_load = sector_count * 512`.
5. Convert the boot image start to BIOS LBA:
   * `boot_image_lba512 = load_rba2048 * 4`
6. Read exactly `sector_count` **512-byte** sectors starting at `boot_image_lba512` into memory at
   `dst`.

Important subtlety:

* `sector_count` is in **512-byte units**
* `load_rba` is in **2048-byte units**

This is why the conversion step above matters.

After loading, BIOS transfers control to the boot image at `CS:IP = load_segment:0000` (physical
`dst`), with `DL` set to the BIOS drive number of the boot device (see below).

In Aero, the real-mode register state at entry is:

* `CS = load_segment`, `IP = 0x0000`
* `DL = boot_drive` (typically `0xE0` for the first CD-ROM)
* `DS = ES = SS = 0x0000`
* `SP = 0x7C00`

---

## 5) Drive number conventions (Aero BIOS)

Aero uses BIOS drive numbers that match common PC conventions:

* **Hard disks:** `0x80`, `0x81`, …
* **CD-ROM (El Torito boot device):** `0xE0`, `0xE1`, …

When booting from a CD, the El Torito boot image expects to find the CD drive number in **`DL`**
when it starts executing.

Note: Aero’s BIOS currently models **exactly one** INT 13h “boot device” at a time as selected by
`BiosConfig::boot_drive` / `DL`. In particular:

* Only the selected CD drive number (typically `0xE0`) is reported present in the CD drive range.
* When booting from CD (`DL=0xE0..=0xEF`), the BIOS reports **no fixed disks** via the BDA and HDD
  `INT 13h` drive numbers (`0x80..`) are not present.

---

## 6) INT 13h calls required for Windows-style boot

Windows boot code (both HDD and CD paths) commonly relies on **INT 13h Extensions** (EDD) rather
than CHS reads. The minimum required calls are:

* `AH=41h` — Extensions check
* `AH=42h` — Extended read (Disk Address Packet)
* `AH=48h` — Extended get drive parameters

Implementation reference: `crates/firmware/src/bios/interrupts.rs::handle_int13` (match arms
`0x41`, `0x42`, `0x48`).

### Supported INT 13h functions for CD-ROM drives (Aero BIOS)

When `DL` is a CD drive number (`0xE0..=0xEF`), Aero’s BIOS implements a minimal, read-oriented INT
13h surface:

| AH | Function | Notes |
|---:|---|---|
| `00h` | Reset disk system | Supported. |
| `01h` | Get status of last operation | Supported. |
| `03h` | Write sectors (CHS) | Not supported; returns write-protected (`CF=1`, `AH=03h`). |
| `05h` | Format track (CHS) | Not supported; returns write-protected (`CF=1`, `AH=03h`). |
| `15h` | Get disk type | Supported; reports presence and returns sector count in **2048-byte sectors**. |
| `41h` | Extensions check (EDD) | Supported; reports EDD 3.0 and `42h`+`48h` support. |
| `42h` | Extended read (DAP) | Supported (read-only). For CD drives, `LBA` + `count` are in **2048-byte sectors**. |
| `43h` | Extended write (DAP) | Not supported; returns write-protected (`CF=1`, `AH=03h`). |
| `48h` | Extended get drive parameters | Supported; reports `bytes_per_sector = 2048` and total sectors in 2048-byte units. |
| `4Bh` | El Torito disk emulation services | Partially supported (only when booted via El Torito). |
| other | Legacy CHS, etc. | Not supported; returns `CF=1`, `AH=01h`. |

### 6.1 AH=41h — Extensions check

Inputs:

* `AH=0x41`
* `BX=0x55AA`
* `DL=drive`

Required outputs on success:

* `CF=0`
* `BX=0xAA55` (signature echoed back swapped)
* `AH=0x30` (report EDD 3.0)
* `CX` feature bits:
  * bit 0 (`0x0001`): extended disk access (`AH=42h`)
  * bit 2 (`0x0004`): extended drive parameters (`AH=48h`)

### 6.2 AH=42h — Extended read (Disk Address Packet)

Inputs:

* `AH=0x42`
* `DL=drive`
* `DS:SI` points to a **Disk Address Packet** (DAP)

DAP formats we accept:

**16-byte DAP (size `0x10`)**

| Offset | Size | Field |
|---:|---:|---|
| `0x00` | 1 | size (`0x10`) |
| `0x01` | 1 | reserved (must be `0`) |
| `0x02` | 2 | sector count (`u16` LE, must be non-zero) |
| `0x04` | 2 | buffer offset |
| `0x06` | 2 | buffer segment |
| `0x08` | 8 | starting LBA (`u64` LE) |

**24-byte DAP (size `0x18`)**

Same as above, plus:

| Offset | Size | Field |
|---:|---:|---|
| `0x10` | 8 | optional 64-bit flat buffer pointer (if non-zero, overrides segment:offset) |

Semantics:

* Reads `count` sectors starting at `lba` into the destination buffer.
  * For **CD drives** (`DL=0xE0..`): `lba` and `count` are in **2048-byte sectors** (ISO logical
    blocks), and the transfer size is `count * 2048` bytes.
* Error handling must set `CF=1` and return an INT 13h status code in `AH`.

#### Sector-size rule (HDD vs CD-ROM)

In Aero, the “sector” unit for the DAP depends on the drive class:

* **HDD (`DL=0x80..=0xDF`)**: DAP `count`/`lba` are in **512-byte sectors** (standard EDD behavior).
* **CD-ROM (`DL=0xE0..=0xEF`)**: DAP `count`/`lba` are in **2048-byte sectors**.
  * Internally, the BIOS reads from a 2048-byte-sector `CdromDevice` backend when provided.
    Otherwise it falls back to reading raw ISO bytes from a 512-byte-sector [`BlockDevice`] and
    converts:
    * `lba512 = lba2048 * 4`
    * `count512 = count2048 * 4`

This matches the `AH=48h` “bytes per sector” value for the drive (see below) and is required for
Windows install-media bootloaders that read from CD-ROM via EDD.

### 6.3 AH=48h — Extended get drive parameters

Inputs:

* `AH=0x48`
* `DL=drive`
* `DS:SI` points to a caller-allocated buffer whose first `u16` is the buffer size in bytes.

Minimum behavior:

* Require `buffer_size >= 0x1A`.
* Fill the EDD parameter table fields needed by Windows boot code (in particular, **bytes per
  sector** and **total sector count**).
    * For **CD drives**, this means reporting `bytes_per_sector = 2048` and `total_sectors` in
      **2048-byte units**.

### 6.4 AH=4Bh — El Torito disk emulation services (compatibility)

Some El Torito boot images query El Torito metadata via INT 13h `AH=4Bh`. Windows 7 install media does
not typically require this path, but Aero implements a minimal subset for compatibility when booting
via El Torito (since it is closely tied to boot-catalog parsing).

Constraints:

* Only available when the BIOS actually booted via El Torito and captured boot-catalog metadata
  during POST.
* Only valid for the El Torito boot drive (must match the `DL` used to boot, typically `0xE0`).

Supported subfunctions (selected by `AL`; Aero supports only these `AX` values):

* `AX=4B00h` (`AL=00h`) — Terminate disk emulation
  * For **no-emulation** boots, Aero treats this as a no-op success.
* `AX=4B01h` (`AL=01h`) — Get disk emulation status
  * Writes a status packet at `ES:DI` (caller provides the buffer).
  * Compatibility rule: if the caller sets the first byte to a non-zero buffer size, Aero requires
    it to be `>= 0x13`.

All other subfunctions are unsupported.

#### Status packet layout (0x13 bytes)

All multi-byte fields are little-endian.

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| `0x00` | 1 | packet size | `0x13` |
| `0x01` | 1 | media type | `0x00` = no-emulation |
| `0x02` | 1 | boot drive | `DL` value used for El Torito boot (typically `0xE0`) |
| `0x03` | 1 | controller index | currently `0` |
| `0x04` | 4 | boot image LBA | ISO LBA (**2048-byte units**) of the boot image (`u32` LE) |
| `0x08` | 4 | boot catalog LBA | ISO LBA (**2048-byte units**) of the boot catalog (`u32` LE) |
| `0x0C` | 2 | load segment | real-mode segment used to load boot image (e.g. `0x07C0`) |
| `0x0E` | 2 | sector count | number of **512-byte** sectors loaded for the initial image |
| `0x10` | 3 | reserved | zero |

`boot image LBA` and `boot catalog LBA` use **ISO logical block addressing** (2048-byte sectors, the
same unit as ISO9660 and the El Torito boot catalog). If you need underlying 512-byte LBAs:
`lba512 = lba2048 * 4` (only relevant when the ISO is exposed via a 512-byte-sector `BlockDevice`).

---

## 7) Note on `-boot-info-table` (mkisofs/xorriso)

Tools like `mkisofs`/`xorriso` support `-boot-info-table`, which patches a “boot info table” into
the boot image for the benefit of **some bootloaders** (historically `isolinux`-style).

Important:

* **Aero BIOS does not consume `-boot-info-table`.**
* It is an optional bootloader-side convenience, not part of El Torito catalog discovery.
