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
- `Blob` is a `REG_BINARY` payload produced by CryptoAPI for the registry-backed certificate store entry
  (**not guaranteed to be raw DER**). To generate the exact bytes, use `tools/win-certstore-regblob-export`
  on Windows and then apply the resulting `.reg`/JSON patch to the offline hive.

### Where the SOFTWARE hive comes from (Win7 ISO)

In a *booted / installed* Windows directory tree, the hive is:

- `Windows\System32\config\SOFTWARE`

In a stock Windows 7 ISO, that file lives **inside WIMs**:

- `sources/install.wim` (the installed OS image)
- `sources/boot.wim` (WinPE / Windows Setup environment)

To patch those, mount the WIM (DISM on Windows, or `wimlib-imagex` cross-platform), then use the mounted
`Windows\System32\config\SOFTWARE` path with the commands below.

### Generate a concrete `.reg` for an offline SOFTWARE hive

To avoid guessing the registry representation, generate the `.reg` using CryptoAPI on Windows:

```powershell
cd tools\win-certstore-regblob-export
cargo build --release
```

#### Windows (reg.exe / reg import)

Generate a `.reg` that targets a hive loaded under `HKLM\\OFFSOFT`:

```bat
tools\win-certstore-regblob-export\target\release\win-certstore-regblob-export.exe ^
  --store ROOT --store TrustedPublisher ^
  --format reg ^
  --reg-hklm-subkey OFFSOFT ^
  path\to\aero.cer > aero-cert.reg
```

Apply to an **offline** `SOFTWARE` hive (run in an elevated Administrator shell):

```bat
reg load HKLM\OFFSOFT X:\path\to\Windows\System32\config\SOFTWARE
reg import aero-cert.reg
reg unload HKLM\OFFSOFT
```

#### Linux/macOS (hivexregedit)

Generate a `.reg` that targets a hive named `SOFTWARE` (run the exporter on Windows, then copy the file):

```sh
# The exporter defaults to --reg-hklm-subkey SOFTWARE, producing keys like:
#   [HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\SystemCertificates\\...]
```

Then merge into the offline hive:

```sh
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\SOFTWARE' /path/to/SOFTWARE aero-cert.reg
```

---

## 2) BCD patches (offline BCD registry hives)

Windows Boot Configuration Data (BCD) stores are registry-hive-like files:

- BIOS: `boot/BCD`
- UEFI: `efi/microsoft/boot/BCD`
- Template (used by Setup to create new stores): `Windows/System32/Config/BCD-Template`
  - On install media, this file is **inside** `sources/install.wim` (mount an index to access it).

These patches set OS-loader booleans using their element IDs:

- `testsigning` → `BcdOSLoaderBoolean_AllowPrereleaseSignatures` (`0x16000049`)
- `nointegritychecks` → `BcdOSLoaderBoolean_DisableIntegrityChecks` (`0x16000048`)

Both patches target the well-known BCD “library settings” objects which Win7 loader entries commonly inherit:

- `{globalsettings}` (GUID `{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}`)
- `{bootloadersettings}` (GUID `{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}`)

For maximum robustness across OEM media, you may also need to set these elements on the actual OS loader objects;
see `docs/win7-bcd-offline-patching.md`.

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
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' /path/to/BCD tools/win7-slipstream/patches/bcd-testsigning.reg
hivexregedit --merge --prefix 'HKEY_LOCAL_MACHINE\BCD' /path/to/BCD tools/win7-slipstream/patches/bcd-nointegritychecks.reg
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
hivexregedit --export --prefix 'HKEY_LOCAL_MACHINE\SOFTWARE' /path/to/SOFTWARE '\Microsoft\SystemCertificates' | rg -i "<THUMBPRINT>"
```

### Verify BCD flags

On Windows:

```bat
bcdedit /store X:\path\to\BCD /enum {default} /v
bcdedit /store X:\path\to\BCD /enum {globalsettings} /v
bcdedit /store X:\path\to\BCD /enum {bootloadersettings} /v
```

You should see:

- `testsigning                 Yes`
- `nointegritychecks           Yes`

(Depending on how the store is structured, you may also see these show up when enumerating `{default}` because of inheritance.)

If `{default}` is missing or you’re not sure which entry Windows will boot, enumerate everything and look for the relevant `Windows Boot Loader` entry:

```bat
bcdedit /store X:\path\to\BCD /enum all /v
```

You can also query the loaded hive directly (after `reg load HKLM\\BCD ...`):

```bat
reg query HKLM\BCD\Objects\{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}\Elements\16000049 /v Element
reg query HKLM\BCD\Objects\{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}\Elements\16000048 /v Element
reg query HKLM\BCD\Objects\{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}\Elements\16000049 /v Element
reg query HKLM\BCD\Objects\{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}\Elements\16000048 /v Element
```

---

## Caveats

- These settings lower boot-time driver security. Use only for development/test images.
- When patching files extracted from an ISO, make sure the files are writable (e.g. ISO extractors often mark files read-only).
- Some images have multiple BCD stores (BIOS + UEFI + templates). Patch all stores you intend to boot from.
