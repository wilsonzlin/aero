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
* **Internally (firmware `BlockDevice`):** the disk backend is **512-byte sectors**.

For CD boot and CD INT 13h I/O, this creates a critical conversion rule between **2048-byte LBAs**
and the underlying 512-byte sector backend:

* `lba512 = lba2048 * 4` (and similarly `count512 = count2048 * 4`)

You will see this conversion repeatedly:

* ISO9660 volume descriptors start at **ISO LBA 16** → underlying 512-sector LBA `16 * 4 = 64`.
* The Boot Record volume descriptor’s **boot catalog pointer** is an **ISO LBA**.
* The boot catalog initial entry’s **`load_rba`** is an **ISO LBA**.
* The boot catalog initial entry’s **`sector_count`** is in **512-byte sectors** (already the
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
   2. Validate `vd[1..6] == "CD001"` (standard identifier).
   3. Dispatch on `vd[0]` (volume descriptor type):
      * `0x00` → **Boot Record VD** (El Torito is identified by a magic string; see below)
      * `0x01` → Primary Volume Descriptor (not used for El Torito discovery)
      * `0x02` → Supplementary (Joliet) (not used for El Torito discovery)
      * `0xFF` → Volume Descriptor Set Terminator (**stop scanning**)
      * other types are ignored
2. If we hit the terminator without finding a valid El Torito Boot Record VD, the ISO is treated as
   **not BIOS-bootable via El Torito**.

> Practical guardrail: cap the scan to a small max (e.g. 64 descriptors) to avoid pathological
> images. Windows media has the Boot Record early.

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

## 3) Boot Catalog (validation entry + initial/default entry)

The **Boot Catalog** is a 2048-byte sector (and may extend beyond one sector if it contains many
entries). Aero’s minimal implementation assumes the first sector contains:

1. A **Validation Entry** (32 bytes) at offset `0x00`
2. An **Initial/Default Entry** (32 bytes) at offset `0x20`

Entries are 32 bytes each.

### 3.1 Validation Entry (required)

The Validation Entry is a fixed-format 32-byte record that validates the catalog itself.

| Offset | Size | Field | Required value / rule |
|---:|---:|---|---|
| `0x00` | 1 | Header ID | Must be `0x01` |
| `0x01` | 1 | Platform ID | Common values: `0x00` x86, `0xEF` EFI. Aero only needs x86. |
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

### 3.2 Initial/Default Entry (required)

The Initial/Default Entry specifies the boot image location and how to load it.

| Offset | Size | Field | Notes |
|---:|---:|---|---|
| `0x00` | 1 | Boot Indicator | `0x88` = bootable, `0x00` = not bootable |
| `0x01` | 1 | Boot Media Type | `0x00` = **no emulation** (required for Win7) |
| `0x02` | 2 | Load Segment | `u16` LE. If `0`, BIOS must default to **`0x07C0`** |
| `0x04` | 1 | System Type | Ignored by Aero for no-emulation |
| `0x05` | 1 | Unused | Must be ignored |
| `0x06` | 2 | Sector Count | `u16` LE, **count of 512-byte sectors to load** |
| `0x08` | 4 | Load RBA | `u32` LE, **ISO LBA (2048-byte)** of boot image |
| `0x0C` | 20 | Unused | Must be ignored |

Minimal acceptance rules:

* Bootable (`boot_indicator == 0x88`)
* No-emulation (`boot_media_type == 0x00`)

Other media types (floppy/hdd emulation) and sectioned catalogs are intentionally unsupported in
the minimal path.

---

## 4) No-emulation boot image loading rules (Win7 critical)

Given the initial/default entry:

1. Compute `load_segment`:
   * If `load_segment != 0`, use it.
   * If `load_segment == 0`, default to **`0x07C0`**.
2. Compute destination physical address: `dst = load_segment << 4`.
3. Compute how many bytes to read: `bytes_to_load = sector_count * 512`.
4. Convert the boot image start to BIOS LBA:
   * `boot_image_lba512 = load_rba2048 * 4`
5. Read exactly `sector_count` **512-byte** sectors starting at `boot_image_lba512` into memory at
   `dst`.

Important subtlety:

* `sector_count` is in **512-byte units**
* `load_rba` is in **2048-byte units**

This is why the conversion step above matters.

After loading, BIOS transfers control to the boot image at `CS:IP = load_segment:0000` (physical
`dst`), with `DL` set to the BIOS drive number of the boot device (see below).

---

## 5) Drive number conventions (Aero BIOS)

Aero uses BIOS drive numbers that match common PC conventions:

* **Hard disks:** `0x80`, `0x81`, …
* **CD-ROM (El Torito boot device):** `0xE0`, `0xE1`, …

When booting from a CD, the El Torito boot image expects to find the CD drive number in **`DL`**
when it starts executing.

---

## 6) INT 13h calls required for Windows-style boot

Windows boot code (both HDD and CD paths) commonly relies on **INT 13h Extensions** (EDD) rather
than CHS reads. The minimum required calls are:

* `AH=41h` — Extensions check
* `AH=42h` — Extended read (Disk Address Packet)
* `AH=48h` — Extended get drive parameters

Implementation reference: `crates/firmware/src/bios/interrupts.rs::handle_int13` (match arms
`0x41`, `0x42`, `0x48`).

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
* Error handling must set `CF=1` and return an INT 13h status code in `AH`.

### 6.3 AH=48h — Extended get drive parameters

Inputs:

* `AH=0x48`
* `DL=drive`
* `DS:SI` points to a caller-allocated buffer whose first `u16` is the buffer size in bytes.

Minimum behavior:

* Require `buffer_size >= 0x1A`.
* Fill the EDD parameter table fields needed by Windows boot code (in particular, **bytes per
  sector** and **total sector count**).

---

## 7) Note on `-boot-info-table` (mkisofs/xorriso)

Tools like `mkisofs`/`xorriso` support `-boot-info-table`, which patches a “boot info table” into
the boot image for the benefit of **some bootloaders** (historically `isolinux`-style).

Important:

* **Aero BIOS does not consume `-boot-info-table`.**
* It is an optional bootloader-side convenience, not part of El Torito catalog discovery.
