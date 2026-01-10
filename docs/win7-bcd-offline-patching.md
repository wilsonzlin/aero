# Windows 7 BCD offline patching (testsigning / nointegritychecks)

This project builds and boots Windows 7 images in automated tests. For those images to work with our performance-critical custom drivers (virtio storage/network/etc.), we patch **Boot Configuration Data (BCD)** stores **offline** (i.e. by editing the BCD files directly, not by running `bcdedit` inside a running Windows install).

The goal of the patch is to make Windows boot with:

- **`testsigning` enabled** (allow loading *test-signed* kernel-mode code), and/or
- **`nointegritychecks` enabled** (disable kernel-mode integrity checks / signature enforcement).

## Motivation

Windows 7 **x64** enforces kernel-mode code signing. In practice:

- Our custom drivers (virtio-blk/virtio-net/virtio-gpu, etc.) will often be **unsigned** during development, or only **test-signed**.
- Without `testsigning` and/or `nointegritychecks`, Windows 7 x64 will refuse to load those drivers, which can prevent the system from booting (storage driver), networking from working, or graphics acceleration from loading.

Patching the BCD stores **before boot** makes the test images deterministic and avoids having to manually toggle boot options inside a guest OS.

## Which files to patch

We patch **all BCD stores that can participate in the boot flow** for our test images. In practice that means:

### Extracted ISO (installation media / WinPE)

Patch both of these (they are used for different firmware boot paths):

- `boot/BCD` (BIOS/CSM boot path)
- `efi/microsoft/boot/BCD` (UEFI boot path)

### Extracted OS image (installed OS template)

Patch:

- `Windows/System32/Config/BCD-Template`

`BCD-Template` is the registry-hive template Windows Setup uses when creating the installed system’s BCD store (e.g. the eventual `\Boot\BCD`). If we don’t patch the template, an installed image can “lose” the settings even if the installer media BCD was patched.

## BCD internals (minimum needed for offline patching)

BCD stores are **registry hives** in standard `REGF` format (the same on-disk format used for `SYSTEM`, `SOFTWARE`, etc.). This is why offline patching can be implemented as “edit a registry hive file”.

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

- The `{GUID}` directory names are the object identifiers (what `bcdedit` displays as `{...}` entries).
- Each element is keyed by its **32-bit element type ID**, rendered as **8 hex digits** (zero-padded) for the subkey name.
- The element payload is stored in the `Element` value under that element-type key.

## Element type IDs to set

The two BCD elements we care about are:

| Element type ID | Elements subkey | `bcdedit` name | Common symbolic name |
| --- | --- | --- | --- |
| `0x16000048` | `16000048` | `nointegritychecks` | `DisableIntegrityChecks` |
| `0x16000049` | `16000049` | `testsigning` | `AllowPrereleaseSignatures` |

These are **Library Boolean** elements (they live in the `0x16xxxxxx` range).

Implementation detail (important for an offline patcher): element data in BCD hives is
stored as `REG_BINARY`. For these Win7 boolean elements, the simplest working encoding
is a 4-byte little-endian integer written into the `Element` value:

- enabled: `01 00 00 00`
- disabled: `00 00 00 00`

This matches the audited offline `.reg` patches under `tools/win7-slipstream/patches/`,
which use `hex:01,00,00,00` for both `testsigning` and `nointegritychecks`.

## Which BCD objects should be patched

To make the setting effective across the different boot paths Windows 7 uses, patch multiple objects (when present):

1. **`{globalsettings}`**
   - Central “library settings” object. Other objects can inherit from it.
   - In the on-disk hive this is typically the object with key name:
     - `{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}`
2. **`{bootloadersettings}`**
   - Another “library settings” object commonly inherited by OS loader entries.
   - In the on-disk hive this is typically the object with key name:
     - `{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}`
3. **All OS loader (`winload*`) entries**
   - Patch the actual boot application entries to be robust even if inheritance differs between media templates.

### Locating OS loader entries programmatically (offline)

On Windows 7, OS loader objects include an application path element that points at `winload.exe` (BIOS) or `winload.efi` (UEFI).

To find them offline:

- Enumerate `Objects\{GUID}` under the BCD hive.
- For each object, look for the element:
  - **`BcdLibraryString_ApplicationPath`** (`0x12000002`)
  - (i.e. `Objects\{GUID}\Elements\12000002\Element`)
- Decode the element’s string value and check whether it contains **`winload`** (case-insensitive substring match is sufficient in practice).

Any object whose `BcdLibraryString_ApplicationPath` contains `winload` should be treated as an OS loader entry and receive the `testsigning` / `nointegritychecks` boolean elements.

## Verification recipe

To verify a patched store from a Windows host (no VM required), use `bcdedit` against the file directly:

```bat
bcdedit /store <path-to-BCD> /enum all
```

In the output, confirm the relevant entries contain:

- `testsigning              Yes`
- `nointegritychecks        Yes`

If those settings show up as `No` or are missing entirely, the offline patch did not apply to the object(s) Windows is actually booting through (most commonly: only patching one of the ISO BCD stores, or patching settings objects but not the OS loader objects themselves).
