# Win7 slipstream registry/BCD patches (Aero)

This directory contains auditable `.reg` patch files for **offline** Windows 7 images:

- **`cert-root-and-trustedpublisher.reg`**: template for installing the Aero public signing certificate into the machine-wide certificate stores (ROOT + TrustedPublisher) inside an *offline* `SOFTWARE` hive.
- **`bcd-testsigning.reg`**: enables BCD `testsigning` (allows test-signed kernel drivers).
- **`bcd-nointegritychecks.reg`**: enables BCD `nointegritychecks` (disables some code integrity checks).

The goal is to support both:

- **Windows-native** workflows (`reg.exe` + `bcdedit.exe`)
- **Cross-platform** workflows (`hivexregedit --merge`)

---

## 1) Certificate patch (offline SOFTWARE hive)

Windows stores machine certificate trust for the OS in:

- `HKLM\SOFTWARE\Microsoft\SystemCertificates\ROOT\Certificates\<SHA1_THUMBPRINT>`
- `HKLM\SOFTWARE\Microsoft\SystemCertificates\TrustedPublisher\Certificates\<SHA1_THUMBPRINT>`

Where:

- `<SHA1_THUMBPRINT>` is the **SHA-1** hash of the certificate’s **DER** bytes, encoded as **40 hex chars**.
- `Blob` is the certificate’s **DER** bytes (`REG_BINARY`) encoded as `.reg` `hex:` data.

### Where the SOFTWARE hive comes from (Win7 ISO)

In a *booted / installed* Windows directory tree, the hive is:

- `Windows\System32\config\SOFTWARE`

In a stock Windows 7 ISO, that file lives **inside WIMs**:

- `sources/install.wim` (the installed OS image)
- `sources/boot.wim` (WinPE / Windows Setup environment)

To patch those, mount the WIM (DISM on Windows, or `wimlib-imagex` cross-platform), then use the mounted
`Windows\System32\config\SOFTWARE` path with the commands below.

### Generate a concrete `.reg` from a `.cer`

Use `tools/win7-slipstream/scripts/cert-to-reg.py` to generate an importable `.reg` with the correct thumbprint + Blob.

#### Windows (reg.exe / reg import)

Generate a `.reg` that targets a hive loaded under `HKLM\\OFFSOFT`:

```bat
py -3 tools\win7-slipstream\scripts\cert-to-reg.py ^
  --mount-key OFFSOFT ^
  --out aero-cert.reg ^
  path\to\aero.cer
```

Apply to an **offline** `SOFTWARE` hive (run in an elevated Administrator shell):

```bat
reg load HKLM\OFFSOFT X:\path\to\Windows\System32\config\SOFTWARE
reg import aero-cert.reg
reg unload HKLM\OFFSOFT
```

#### Linux/macOS (hivexregedit)

Generate a `.reg` that targets a hive named `SOFTWARE` (first component after `HKLM\\` is ignored by some tools and required by others, so use the conventional hive name here):

```sh
python3 tools/win7-slipstream/scripts/cert-to-reg.py \
  --mount-key SOFTWARE \
  --out aero-cert.reg \
  /path/to/aero.cer
```

Then merge into the offline hive:

```sh
hivexregedit --merge /path/to/SOFTWARE aero-cert.reg
```

---

## 2) BCD patches (offline BCD registry hives)

Windows Boot Configuration Data (BCD) stores are registry-hive-like files:

- BIOS: `boot/BCD`
- UEFI: `efi/microsoft/boot/BCD`
- Template (used by Setup to create new stores): `boot/BCD-Template` (sometimes also under `efi/...`)

These patches set OS-loader booleans using their element IDs:

- `testsigning` → `BcdOSLoaderBoolean_AllowPrereleaseSignatures` (`0x16000049`)
- `nointegritychecks` → `BcdOSLoaderBoolean_DisableIntegrityChecks` (`0x16000048`)

Both patches target the well-known object `{bootloadersettings}` (GUID `6efb52bf-1766-41db-a6b3-0ee5eff72bd7`),
because typical Win7 OS loader entries (including WinPE) **inherit** from it.

### Windows (reg.exe / reg import)

Run in an elevated Administrator shell:

```bat
reg load HKLM\BCD X:\path\to\BCD
reg import tools\win7-slipstream\patches\bcd-testsigning.reg
reg import tools\win7-slipstream\patches\bcd-nointegritychecks.reg
reg unload HKLM\BCD
```

### Linux/macOS (hivexregedit)

```sh
hivexregedit --merge /path/to/BCD tools/win7-slipstream/patches/bcd-testsigning.reg
hivexregedit --merge /path/to/BCD tools/win7-slipstream/patches/bcd-nointegritychecks.reg
```

---

## 3) Verification

### Verify certificate in the offline SOFTWARE hive

On Windows (after `reg load HKLM\\OFFSOFT ...`):

```bat
reg query HKLM\OFFSOFT\Microsoft\SystemCertificates\ROOT\Certificates\<THUMBPRINT> /v Blob
reg query HKLM\OFFSOFT\Microsoft\SystemCertificates\TrustedPublisher\Certificates\<THUMBPRINT> /v Blob
```

On Linux/macOS:

```sh
hivexregedit --export /path/to/SOFTWARE | rg -i "<THUMBPRINT>"
```

### Verify BCD flags

On Windows:

```bat
bcdedit /store X:\path\to\BCD /enum {bootloadersettings} /v
```

You should see:

- `testsigning                 Yes`
- `nointegritychecks           Yes`

(Depending on how the store is structured, you may also see these show up when enumerating `{default}` because of inheritance.)

You can also query the loaded hive directly (after `reg load HKLM\\BCD ...`):

```bat
reg query HKLM\BCD\Objects\{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}\Elements\16000049 /v Element
reg query HKLM\BCD\Objects\{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}\Elements\16000048 /v Element
```

---

## Caveats

- These settings lower boot-time driver security. Use only for development/test images.
- When patching files extracted from an ISO, make sure the files are writable (e.g. ISO extractors often mark files read-only).
- Some images have multiple BCD stores (BIOS + UEFI + templates). Patch all stores you intend to boot from.
