# `bcd_patch`

Cross-platform *offline* patching of Windows 7 BCD stores by directly editing the BCD REGF hive.

## Why a REGF editor?

Windows stores the Boot Configuration Data (BCD) database as a registry hive file. The standard
tooling (`bcdedit.exe`) requires Windows APIs and cannot be used in Linux/macOS CI.

This crate uses the pure-Rust [`regf`](https://crates.io/crates/regf) library to read/modify/write
REGF hives without any Windows-only dependencies.

## Usage

```bash
bcd_patch --store boot/BCD

# Explicitly disable one of the flags:
bcd_patch --store boot/BCD --testsigning off
```

To patch all three standard store locations inside an extracted Windows 7 media tree:

```bash
bcd_patch win7-tree --root /path/to/extracted/win7
```

Flags default to **on** unless explicitly set to **off**:

- `testsigning` → BCD element type `0x16000049` (BcdLibraryBoolean_AllowPrereleaseSignatures)
- `nointegritychecks` → BCD element type `0x16000048` (BcdLibraryBoolean_DisableIntegrityChecks)

## What gets patched (object selection)

`bcd_patch` edits the offline hive structure:

`root -> Objects -> {object-guid} -> Elements -> {element-type-hex} -> value "Element"`

The tool enables/disables the target boolean elements on:

1. `{globalsettings}` if present (`{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}`)
2. `{bootloadersettings}` if present (`{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}`)
3. Any object whose `BcdLibraryString_ApplicationPath` (`0x12000002`) contains `winload` or
   `winresume` (e.g. `\\Windows\\System32\\winload.exe`, `winload.efi`)
4. Additionally, objects referenced by the `{bootmgr}` display order (`0x24000001`) and default
   entry (`0x23000003`) are included when those elements exist.

## Windows verification (manual)

After patching a store file, validate it on Windows:

```cmd
bcdedit /store C:\path\to\BCD /enum all
```

Look for `testsigning Yes` and/or `nointegritychecks Yes` on the relevant boot loader entries.
