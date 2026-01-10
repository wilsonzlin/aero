# Windows 7 BCD offline patching (testsigning / nointegritychecks)

This project builds and boots Windows 7 images in automated tests. For those images to work with
our performance-critical custom drivers (virtio storage/network/etc.), we patch **Boot
Configuration Data (BCD)** stores **offline** (i.e. by editing the BCD files directly, not by
running `bcdedit` inside a running Windows install).

The goal of the patch is to make Windows boot with:

- **`testsigning` enabled** (allow loading *test-signed* kernel-mode code), and/or
- **`nointegritychecks` enabled** (disable kernel-mode integrity checks / signature enforcement).

This document exists so we do **not** rely on “heuristics + confirm later” when patching a
boot-critical registry hive.

> Safety: Always take a backup copy of the BCD store before patching. A corrupted BCD can prevent
> a system or image from booting.

---

## Motivation

Windows 7 **x64** enforces kernel-mode code signing. In practice:

- Our custom drivers (virtio-blk/virtio-net/virtio-gpu, etc.) will often be **unsigned** during
  development, or only **test-signed**.
- Without `testsigning` and/or `nointegritychecks`, Windows 7 x64 will refuse to load those
  drivers, which can prevent the system from booting (storage driver), networking from working,
  or graphics acceleration from loading.

Patching the BCD stores **before boot** makes test images deterministic and avoids having to
manually toggle boot options inside a guest OS.

---

## Which files to patch (Win7)

We patch **all BCD stores that can participate in the boot flow** for our test images. In
practice that means:

### Extracted ISO (installation media / WinPE)

Patch both of these (they are used for different firmware boot paths):

- `boot/BCD` (BIOS/CSM boot path)
- `efi/microsoft/boot/BCD` (UEFI boot path)

Note: ISO extractors and non-Windows filesystems may change the case of these
paths (e.g. `EFI/Microsoft/Boot/bcd`). Implementations should treat the path
lookup as case-insensitive.

Also note that on Windows these files are often marked hidden/system/read-only.
An offline patcher must ensure the files are writable before attempting to write
them back.

### Extracted OS image (installed OS template)

Patch:

- `Windows/System32/Config/BCD-Template`

`BCD-Template` is the registry-hive template Windows Setup uses when creating the installed
system’s BCD store (e.g. the eventual `\\Boot\\BCD`). If we don’t patch the template, an installed
image can “lose” the settings even if the installer media BCD was patched.

---

## BCD internals (minimum needed for offline patching)

BCD stores are **registry hives** in standard `REGF` format (the same on-disk format used for
`SYSTEM`, `SOFTWARE`, etc.). This is why offline patching can be implemented as “edit a registry
hive file”.

The minimum structure you need to know is:

```
<root>
  Objects
    {GUID}
      Elements
        <8-hex element type>        (key name, 8 hex digits, no `0x` prefix)
          Element                   (value name)
```

Concretely, the offline patcher usually writes values like:

```
Objects\{<object-guid>}\Elements\16000049\Element
Objects\{<object-guid>}\Elements\16000048\Element
```

Notes:

- The `{GUID}` directory names are the object identifiers (what `bcdedit` displays as `{...}`
  entries).
- Each element is keyed by its **32-bit element type ID**, rendered as **8 hex digits**
  (zero-padded) for the subkey name.
- The element payload is stored in the `Element` value under that element-type key.

### Boolean encoding (Win7)

Element data in BCD hives is stored as `REG_BINARY`. For the Win7 boolean elements used by this
tool, the simplest working encoding is:

```
[u32 element_type (LE)] [u32 data_len (LE)] [data...]
```

For these Win7 boolean elements, `data_len` is 4 and the payload is a little-endian u32:

- enabled: `01 00 00 00`
- disabled: `00 00 00 00`

The two boolean element types we care about are:

| Element type ID | Elements subkey | `bcdedit` name | BCD element constant name |
| --- | --- | --- | --- |
| `0x16000048` | `16000048` | `nointegritychecks` | `BcdLibraryBoolean_DisableIntegrityChecks` (`DisableIntegrityChecks`) |
| `0x16000049` | `16000049` | `testsigning` | `BcdLibraryBoolean_AllowPrereleaseSignatures` (`AllowPrereleaseSignatures`) |

So the full `Element` blob is typically 12 bytes, for example:

- `testsigning` (`0x16000049`, enabled):
  - `49 00 00 16  04 00 00 00  01 00 00 00`
- `nointegritychecks` (`0x16000048`, enabled):
  - `48 00 00 16  04 00 00 00  01 00 00 00`

This matches the audited offline `.reg` patches under `tools/win7-slipstream/patches/`.

---

## Well-known object GUIDs (Win7)

These are the canonical GUIDs behind `bcdedit` aliases:

| `bcdedit` alias                | GUID                                  | Notes |
|--------------------------------|---------------------------------------|------|
| `{bootmgr}`                    | `9dea862c-5cdd-4e70-acc1-f32b344d4795` | Boot Manager object. Used to find `default`/`displayorder` entries. |
| `{globalsettings}`             | `7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e` | Global “library” settings inherited by many objects. |
| `{bootloadersettings}`         | `6efb52bf-1766-41db-a6b3-0ee5eff72bd7` | Template inherited by Windows Boot Loader entries. |
| `{resumeloadersettings}`       | `1afa9c49-16ab-4a5c-901b-212802da9460` | Template inherited by Windows Resume Loader entries. |
| `{memdiag}` (commonly present) | `b2721d73-1db4-4c62-bf78-c548a880142d` | Windows Memory Diagnostic. Usually present in boot menu display order. |
| `{ntldr}` (optional)           | `466f5a88-0af2-4f76-9038-095b170dc21c` | Legacy NTLDR entry, only present if created. |

### Reference: audited `.reg` patches in this repo

If you need a concrete, known-good offline patch format, see:

- `tools/win7-slipstream/patches/bcd-testsigning.reg`
- `tools/win7-slipstream/patches/bcd-nointegritychecks.reg`

They patch the well-known settings objects by GUID (see table above) by writing the `Element`
value directly.

Object GUID key names are **case-insensitive**. Some tools show GUIDs with braces (`{...}`) and
some without; offline patching should tolerate both.

---

## Element type IDs used by the offline patcher

BCD “elements” are identified by a 32-bit `BCD_ELEMENT_TYPE` number. In the registry hive, each
element is stored as a subkey under `Elements` named as **8-digit hex** (e.g. `16000048`).

Only the element types actively used by the patcher are listed here:

| Hex element type | Meaning / `bcdedit` name | Data kind  | Used for |
|------------------|--------------------------|-----------|----------|
| `0x16000048`     | `nointegritychecks`      | Boolean   | Disables kernel-mode code integrity checks. |
| `0x16000049`     | `testsigning`            | Boolean   | Allows test-signed / prerelease signatures. |
| `0x12000002`     | `applicationpath`        | String    | Identifies loader objects (`winload.*`, `winresume.*`). |
| `0x23000003`     | `{bootmgr} default`      | Object    | GUID of the default boot entry. |
| `0x24000001`     | `{bootmgr} displayorder` | ObjectList | Ordered list of boot entries shown in the boot menu. |

---

## Deterministic object selection (what gets patched)

To make the setting effective across the different boot paths Windows 7 uses, patch multiple
objects when present:

1. `{globalsettings}`
2. `{bootloadersettings}`
3. `{resumeloadersettings}` (if present)
4. All OS/resume loader entries (`winload*`, `winresume*`) discovered by `applicationpath`
5. Additionally, objects referenced by `{bootmgr}`’s `displayorder`/`default` are included when
   those elements exist.

Note on object identifiers: `bcdedit` supports symbolic names like `{default}` and `{bootmgr}`.
Offline patchers work directly with the REGF hive and therefore typically patch objects by their
GUID keys under `Objects\\{GUID}`. Patching the well-known settings objects and all `winload*`
entries avoids having to resolve store-specific aliases like `{default}`.

### Locating OS loader entries programmatically (offline)

On Windows 7, OS loader objects include an application path element that points at `winload.exe`
(BIOS) or `winload.efi` (UEFI). Resume loader objects similarly reference `winresume.*`.

To find them offline:

- Enumerate `Objects\\{GUID}` under the BCD hive.
- For each object, look for the element:
  - `BcdLibraryString_ApplicationPath` (`0x12000002`)
  - (i.e. `Objects\\{GUID}\\Elements\\12000002\\Element`)
- Decode the element’s string value and check whether it contains `winload` or `winresume`
  (case-insensitive substring match is sufficient in practice).

In practice, `BcdLibraryString_ApplicationPath` is a Windows path such as
`\\Windows\\system32\\winload.exe` / `winload.efi`. If you don’t want to fully decode the
element, scanning the raw bytes for the UTF-16LE substring `w\0i\0n\0l\0o\0a\0d\0` is a pragmatic
approach.

---

## Verification steps (developers with Win7 media)

### 1) Enumerate objects using `bcdedit`

To verify a patched store from a Windows host (no VM required), use `bcdedit` against the file
directly:

```bat
bcdedit /store <path-to-BCD> /enum all /v
```

In the output, confirm the relevant entries contain:

- `testsigning              Yes`
- `nointegritychecks        Yes`

If you specifically patched the settings objects (recommended), you can also check them directly:

```bat
bcdedit /store <path-to-BCD> /enum {globalsettings} /v
bcdedit /store <path-to-BCD> /enum {bootloadersettings} /v
```

For a low-level check of the exact bytes written, you can load the store as a hive (requires an
elevated prompt) and query the element value directly:

```bat
reg load HKLM\BCD <path-to-BCD>
reg query HKLM\BCD\Objects\{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}\Elements\16000049 /v Element
reg query HKLM\BCD\Objects\{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}\Elements\16000048 /v Element
reg unload HKLM\BCD
```

If those settings show up as `No` or are missing entirely, the offline patch did not apply to the
object(s) Windows is actually booting through (most commonly: only patching one of the ISO BCD
stores, or patching settings objects but not the OS loader objects themselves).

### 2) Load the BCD hive and inspect object/element keys

These steps let you confirm both GUID presence and element type IDs on a real Windows 7 BCD store
without committing any binaries to the repo.

Load the hive into a temporary registry key:

```bat
reg.exe load HKLM\BCD_OFFLINE X:\Boot\BCD
```

List well-known object keys (GUIDs):

```bat
reg.exe query HKLM\BCD_OFFLINE\Objects
```

Inspect element type subkeys for an object (example: `{bootloadersettings}`):

```bat
reg.exe query HKLM\BCD_OFFLINE\Objects\{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}\Elements
```

You should see element subkeys named like `16000048`, `16000049`, etc, matching the IDs above.

Unload when done:

```bat
reg.exe unload HKLM\BCD_OFFLINE
```

---

## Implementation linkage

The canonical constants live in:

- `tools/bcd_patch/src/constants.rs`

The patcher’s selection strategy is implemented (and unit tested) in:

- `tools/bcd_patch/src/lib.rs` (`select_target_objects`)
