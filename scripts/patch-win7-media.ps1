#Requires -Version 5.1
#Requires -RunAsAdministrator

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$IsoRoot,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$CertPath,

  # If -CertPath is a .pfx, supply its password via -PfxPassword.
  [string]$PfxPassword,

  # Offline certificate stores to populate in both boot.wim and install.wim.
  # Defaults match the minimum needed for trusting test-signed kernel-mode driver catalogs.
  [string[]]$CertStores = @('ROOT', 'TrustedPublisher'),

  # Optional but default ON for Windows 7 media patching.
  [bool]$EnableNoIntegrityChecks = $true,

  # Patch boot.wim index 1 (WinPE) and 2 (Setup) by default.
  [int[]]$PatchBootWimIndices = @(1, 2),

  # Patch all install.wim indices by default.
  [bool]$PatchInstallWimAllIndices = $true,

  # If -PatchInstallWimAllIndices is $false, specify the subset to patch.
  [int[]]$PatchInstallWimIndices
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Log {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Message,
    [ValidateSet('INFO', 'WARN', 'ERROR')]
    [string]$Level = 'INFO'
  )

  $ts = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
  Write-Host "[$ts] [$Level] $Message"
}

function Assert-Admin {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'This script must be run as Administrator.'
  }
}

function Assert-ToolExists {
  param(
    [Parameter(Mandatory = $true)]
    [string]$ToolName
  )

  if (-not (Get-Command $ToolName -ErrorAction SilentlyContinue)) {
    throw "Required tool not found in PATH: $ToolName"
  }
}

function Normalize-CertStoreName {
  param(
    [Parameter(Mandatory = $true)]
    [string]$StoreName
  )

  $upper = $StoreName.Trim().ToUpperInvariant()
  switch ($upper) {
    'ROOT' { return 'ROOT' }
    'TRUSTEDPUBLISHER' { return 'TrustedPublisher' }
    'TRUSTEDPEOPLE' { return 'TrustedPeople' }
    default { throw "Unsupported certificate store '$StoreName'. Supported values: ROOT, TrustedPublisher, TrustedPeople." }
  }
}

function Normalize-CertStoreList {
  param(
    [Parameter(Mandatory = $true)]
    [string[]]$Stores
  )

  $out = New-Object System.Collections.Generic.List[string]
  foreach ($store in $Stores) {
    if ([string]::IsNullOrWhiteSpace($store)) {
      continue
    }
    $norm = Normalize-CertStoreName -StoreName $store
    if (-not ($out -contains $norm)) {
      $out.Add($norm) | Out-Null
    }
  }

  if ($out.Count -eq 0) {
    throw '-CertStores must contain at least one store.'
  }

  return $out.ToArray()
}

function Ensure-FileWritable {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  $item = Get-Item -LiteralPath $Path -ErrorAction Stop
  if ($item.PSIsContainer) {
    return
  }
  if (($item.Attributes -band [IO.FileAttributes]::ReadOnly) -ne 0) {
    Write-Log "Clearing read-only attribute: $Path"
    $item.Attributes = ($item.Attributes -band (-bnot [IO.FileAttributes]::ReadOnly))
  }
}

function Invoke-Exe {
  param(
    [Parameter(Mandatory = $true)]
    [string]$FilePath,
    [Parameter(Mandatory = $true)]
    [string[]]$ArgumentList
  )

  Write-Log "Running: $FilePath $($ArgumentList -join ' ')"
  $output = & $FilePath @ArgumentList 2>&1
  $exitCode = $LASTEXITCODE
  if ($exitCode -ne 0) {
    $outText = ($output | Out-String).Trim()
    throw "Command failed (exit $exitCode): $FilePath $($ArgumentList -join ' ')`n$outText"
  }
  return $output
}

function Invoke-ExeResult {
  param(
    [Parameter(Mandatory = $true)]
    [string]$FilePath,
    [Parameter(Mandatory = $true)]
    [string[]]$ArgumentList
  )

  Write-Log "Running: $FilePath $($ArgumentList -join ' ')"
  $output = & $FilePath @ArgumentList 2>&1
  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ,$output
  }
}

function Get-WimIndices {
  param(
    [Parameter(Mandatory = $true)]
    [string]$WimFile
  )

  $out = Invoke-Exe -FilePath dism.exe -ArgumentList @(
    '/English',
    '/Get-WimInfo',
    "/WimFile:$WimFile"
  )

  $indices = @()
  foreach ($line in $out) {
    $m = [Regex]::Match($line, '^\s*Index\s*:\s*(\d+)\s*$')
    if ($m.Success) {
      $indices += [int]$m.Groups[1].Value
    }
  }
  if ($indices.Count -eq 0) {
    $outText = ($out | Out-String).Trim()
    throw "Failed to parse WIM indices from DISM output for: $WimFile`n$outText"
  }
  return $indices
}

function Mount-Wim {
  param(
    [Parameter(Mandatory = $true)]
    [string]$WimFile,
    [Parameter(Mandatory = $true)]
    [int]$Index,
    [Parameter(Mandatory = $true)]
    [string]$MountDir
  )

  New-Item -ItemType Directory -Path $MountDir -Force | Out-Null
  Invoke-Exe -FilePath dism.exe -ArgumentList @(
    '/English',
    '/Mount-Wim',
    "/WimFile:$WimFile",
    "/Index:$Index",
    "/MountDir:$MountDir"
  ) | Out-Null
}

function Unmount-Wim {
  param(
    [Parameter(Mandatory = $true)]
    [string]$MountDir,
    [Parameter(Mandatory = $true)]
    [bool]$Commit
  )

  $mode = if ($Commit) { '/Commit' } else { '/Discard' }
  try {
    Invoke-Exe -FilePath dism.exe -ArgumentList @(
      '/English',
      '/Unmount-Wim',
      "/MountDir:$MountDir",
      $mode
    ) | Out-Null
  } finally {
    # Best-effort cleanup of the mount directory (WIM mount points often remain non-empty).
    try {
      Remove-Item -LiteralPath $MountDir -Recurse -Force -ErrorAction Stop
    } catch {
      Write-Log "Failed to remove mount directory (ok to ignore): $MountDir`n$($_.Exception.Message)" 'WARN'
    }
  }
}

function Load-OfflineHive {
  param(
    [Parameter(Mandatory = $true)]
    [string]$HivePath,
    [Parameter(Mandatory = $true)]
    [string]$HiveName
  )

  Ensure-FileWritable -Path $HivePath
  Invoke-Exe -FilePath reg.exe -ArgumentList @(
    'load',
    "HKLM\$HiveName",
    $HivePath
  ) | Out-Null
}

function Unload-OfflineHive {
  param(
    [Parameter(Mandatory = $true)]
    [string]$HiveName
  )

  Invoke-Exe -FilePath reg.exe -ArgumentList @(
    'unload',
    "HKLM\$HiveName"
  ) | Out-Null
}

function Patch-BcdStore {
  param(
    [Parameter(Mandatory = $true)]
    [string]$StorePath,
    [Parameter(Mandatory = $true)]
    [bool]$EnableNoIntegrityChecks
  )

  Ensure-FileWritable -Path $StorePath

  function Get-BcdSettingValue {
    param(
      [Parameter(Mandatory = $true)]
      [string]$EnumText,
      [Parameter(Mandatory = $true)]
      [string]$SettingName
    )

    $m = [Regex]::Match($EnumText, "(?im)^\\s*$SettingName\\s+(?<val>\\S+)")
    if ($m.Success) {
      return $m.Groups['val'].Value
    }
    return $null
  }

  $defaultPatched = $false
  $patchedIds = @()
  $attempt = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', '{default}', 'testsigning', 'on')
  if ($attempt.ExitCode -eq 0) {
    $defaultPatched = $true
    if ($EnableNoIntegrityChecks) {
      Invoke-Exe -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', '{default}', 'nointegritychecks', 'on') | Out-Null
    } else {
      # Explicitly ensure it is not enabled. Prefer deletevalue to keep the store clean, but fall
      # back to an explicit "off" setting if deletevalue fails for any reason.
      $del = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/deletevalue', '{default}', 'nointegritychecks')
      if ($del.ExitCode -ne 0) {
        $setOff = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', '{default}', 'nointegritychecks', 'off')
        if ($setOff.ExitCode -ne 0) {
          $outText = ($setOff.Output | Out-String).Trim()
          throw "Failed to disable nointegritychecks in BCD store: $StorePath`n$outText"
        }
      }
    }
  } else {
    $outText = ($attempt.Output | Out-String).Trim()
    Write-Log "Failed to patch {default} in BCD store; falling back to patching compatible entries from /enum all.`n$outText" 'WARN'

    $ids = Get-BcdIdentifiers -StorePath $StorePath
    if ($ids.Count -eq 0) {
      throw "Unable to enumerate identifiers in BCD store: $StorePath"
    }

    foreach ($id in $ids) {
      $setTestSigning = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', $id, 'testsigning', 'on')
      if ($setTestSigning.ExitCode -ne 0) {
        continue
      }

      if ($EnableNoIntegrityChecks) {
        Invoke-Exe -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', $id, 'nointegritychecks', 'on') | Out-Null
      } else {
        $del = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/deletevalue', $id, 'nointegritychecks')
        if ($del.ExitCode -ne 0) {
          $setOff = Invoke-ExeResult -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/set', $id, 'nointegritychecks', 'off')
          if ($setOff.ExitCode -ne 0) {
            $outText = ($setOff.Output | Out-String).Trim()
            throw "Failed to disable nointegritychecks for $id in BCD store: $StorePath`n$outText"
          }
        }
      }

      $patchedIds += $id
    }

    if ($patchedIds.Count -eq 0) {
      throw "Fallback BCD patching did not find any compatible entries to patch in: $StorePath"
    }
  }

  if ($defaultPatched) {
    $out = Invoke-Exe -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/enum', '{default}')
    $outText = ($out | Out-String)

    $testSigningValue = Get-BcdSettingValue -EnumText $outText -SettingName 'testsigning'
    if ($testSigningValue -ne 'Yes') {
      throw "Failed to verify testsigning=Yes in BCD store: $StorePath`n$outText"
    }

    $noIntegrityValue = Get-BcdSettingValue -EnumText $outText -SettingName 'nointegritychecks'
    if ($EnableNoIntegrityChecks) {
      if ($noIntegrityValue -ne 'Yes') {
        throw "Failed to verify nointegritychecks=Yes in BCD store: $StorePath`n$outText"
      }
    } else {
      if ($noIntegrityValue -eq 'Yes') {
        throw "Failed to verify nointegritychecks is disabled in BCD store: $StorePath`n$outText"
      }
    }
  } else {
    if ($patchedIds.Count -eq 0) {
      throw "BCD store patch fallback did not identify any entries to patch: $StorePath"
    }

    foreach ($id in $patchedIds) {
      $out = Invoke-Exe -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/enum', $id)
      $outText = ($out | Out-String)

      $testSigningValue = Get-BcdSettingValue -EnumText $outText -SettingName 'testsigning'
      if ($testSigningValue -ne 'Yes') {
        throw "Failed to verify testsigning=Yes in BCD store for $id: $StorePath`n$outText"
      }

      $noIntegrityValue = Get-BcdSettingValue -EnumText $outText -SettingName 'nointegritychecks'
      if ($EnableNoIntegrityChecks) {
        if ($noIntegrityValue -ne 'Yes') {
          throw "Failed to verify nointegritychecks=Yes in BCD store for $id: $StorePath`n$outText"
        }
      } else {
        if ($noIntegrityValue -eq 'Yes') {
          throw "Failed to verify nointegritychecks is disabled in BCD store for $id: $StorePath`n$outText"
        }
      }
    }
  }
}

function Get-BcdIdentifiers {
  param(
    [Parameter(Mandatory = $true)]
    [string]$StorePath
  )

  $out = Invoke-Exe -FilePath bcdedit.exe -ArgumentList @('/store', $StorePath, '/enum', 'all')

  $ids = @()
  foreach ($line in $out) {
    $trimmed = $line.Trim()
    if (-not $trimmed) {
      continue
    }
    $m = [Regex]::Match($trimmed, '^identifier\s+(?<id>\{[^\}]+\})\s*$')
    if ($m.Success) {
      $ids += $m.Groups['id'].Value
    }
  }

  return @($ids | Sort-Object -Unique)
}

function Get-CertificateDerBlobs {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [string]$PfxPassword
  )

  $ext = [IO.Path]::GetExtension($Path)
  if ($null -eq $ext) {
    $ext = ''
  }
  $ext = $ext.ToLowerInvariant()

  if ($ext -eq '.pem') {
    $pem = Get-Content -LiteralPath $Path -Raw
    $matches = [Regex]::Matches($pem, '-----BEGIN CERTIFICATE-----\s*(?<b64>.*?)\s*-----END CERTIFICATE-----', [Text.RegularExpressions.RegexOptions]::Singleline)
    if ($matches.Count -eq 0) {
      throw "No PEM certificate blocks found in: $Path"
    }
    foreach ($m in $matches) {
      $b64 = $m.Groups['b64'].Value
      $b64 = ($b64 -replace '\s', '')
      Write-Output -NoEnumerate ([Convert]::FromBase64String($b64))
    }
    return
  }

  if ($ext -eq '.pfx') {
    if ([string]::IsNullOrEmpty($PfxPassword)) {
      throw "CertPath is a .pfx but -PfxPassword was not provided: $Path"
    }
    $coll = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2Collection
    $flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::DefaultKeySet
    $coll.Import($Path, $PfxPassword, $flags)
    if ($coll.Count -eq 0) {
      throw "No certificates found in PFX: $Path"
    }
    foreach ($c in ($coll | Sort-Object Thumbprint -Unique)) {
      Write-Output -NoEnumerate ($c.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert))
    }
    return
  }

  # .cer / .crt / anything else X509Certificate2 can parse.
  $cert2 = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($Path)
  Write-Output -NoEnumerate ($cert2.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert))
}

function Get-CertificateThumbprintHex {
  param(
    [Parameter(Mandatory = $true)]
    [byte[]]$CertificateDer
  )

  $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(,$CertificateDer)
  return $cert.Thumbprint.ToUpperInvariant()
}

# Uses CryptoAPI (crypt32) to write certificate material into the *offline* registry hive
# in a format understood by Windows. This avoids crafting registry blobs by hand.
if (-not ('OfflineCertInjector' -as [type])) {
  Add-Type -Language CSharp -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32;
using Microsoft.Win32.SafeHandles;

public static class OfflineCertInjector
{
    private const uint X509_ASN_ENCODING = 0x00000001;
    private const uint PKCS_7_ASN_ENCODING = 0x00010000;
    private const uint CERT_STORE_ADD_REPLACE_EXISTING = 3;

    // From wincrypt.h: #define CERT_STORE_PROV_SYSTEM_REGISTRY_W ((LPCSTR)13)
    private static readonly IntPtr CERT_STORE_PROV_SYSTEM_REGISTRY = new IntPtr(13);

    // Allow opening the store with whatever rights the key grants.
    private const uint CERT_STORE_MAXIMUM_ALLOWED_FLAG = 0x00001000;

    [DllImport("crypt32.dll", SetLastError = true)]
    private static extern IntPtr CertOpenStore(
        IntPtr lpszStoreProvider,
        uint dwEncodingType,
        IntPtr hCryptProv,
        uint dwFlags,
        IntPtr pvPara
    );

    [DllImport("crypt32.dll", SetLastError = true)]
    private static extern bool CertCloseStore(IntPtr hCertStore, uint dwFlags);

    [DllImport("crypt32.dll", SetLastError = true)]
    private static extern bool CertAddEncodedCertificateToStore(
        IntPtr hCertStore,
        uint dwCertEncodingType,
        byte[] pbCertEncoded,
        int cbCertEncoded,
        uint dwAddDisposition,
        out IntPtr ppCertContext
    );

    [DllImport("crypt32.dll", SetLastError = true)]
    private static extern bool CertFreeCertificateContext(IntPtr pCertContext);

    public static void AddCertificateToLoadedOfflineSoftwareHive(string hiveKeyName, string storeName, byte[] certificateDer)
    {
        if (string.IsNullOrWhiteSpace(hiveKeyName))
            throw new ArgumentException("hiveKeyName is required", "hiveKeyName");
        if (string.IsNullOrWhiteSpace(storeName))
            throw new ArgumentException("storeName is required", "storeName");
        if (certificateDer == null || certificateDer.Length == 0)
            throw new ArgumentException("certificateDer is required", "certificateDer");

        using (RegistryKey hiveRoot = Registry.LocalMachine.OpenSubKey(hiveKeyName, writable: true))
        {
            if (hiveRoot == null)
                throw new InvalidOperationException("Offline SOFTWARE hive not loaded at HKLM\\\\" + hiveKeyName);

            using (RegistryKey storeRoot = hiveRoot.CreateSubKey(@"Microsoft\\SystemCertificates\\" + storeName))
            {
                if (storeRoot == null)
                    throw new InvalidOperationException("Failed to open or create HKLM\\\\" + hiveKeyName + "\\\\Microsoft\\\\SystemCertificates\\\\" + storeName);

                // CryptoAPI expects this layout under the store key, e.g.:
                //   ...\\SystemCertificates\\ROOT\\Certificates\\<thumbprint>\\Blob
                using (storeRoot.CreateSubKey("Certificates")) { }

                IntPtr hCertStore = CertOpenStore(
                    CERT_STORE_PROV_SYSTEM_REGISTRY,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    IntPtr.Zero,
                    CERT_STORE_MAXIMUM_ALLOWED_FLAG,
                    storeRoot.Handle.DangerousGetHandle()
                );

                if (hCertStore == IntPtr.Zero)
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "CertOpenStore(CERT_STORE_PROV_SYSTEM_REGISTRY_W) failed");

                try
                {
                    IntPtr pCertContext;
                    bool ok = CertAddEncodedCertificateToStore(
                        hCertStore,
                        X509_ASN_ENCODING,
                        certificateDer,
                        certificateDer.Length,
                        CERT_STORE_ADD_REPLACE_EXISTING,
                        out pCertContext
                    );
                    if (!ok)
                        throw new Win32Exception(Marshal.GetLastWin32Error(), "CertAddEncodedCertificateToStore failed");

                    if (pCertContext != IntPtr.Zero)
                        CertFreeCertificateContext(pCertContext);
                }
                finally
                {
                    CertCloseStore(hCertStore, 0);
                }
            }
        }
    }
}
"@
}

function Inject-CertIntoOfflineSoftwareHive {
  param(
    [Parameter(Mandatory = $true)]
    [string]$HiveName,
    [Parameter(Mandatory = $true)]
    [byte[]]$CertificateDer,
    [Parameter(Mandatory = $true)]
    [string[]]$Stores
  )

  foreach ($store in $Stores) {
    [OfflineCertInjector]::AddCertificateToLoadedOfflineSoftwareHive($HiveName, $store, $CertificateDer)
  }
}

function Assert-CertPresentInOfflineSoftwareHive {
  param(
    [Parameter(Mandatory = $true)]
    [string]$HiveName,
    [Parameter(Mandatory = $true)]
    [string]$StoreName,
    [Parameter(Mandatory = $true)]
    [string]$ThumbprintHex
  )

  $certKeyPath = "HKLM:\$HiveName\Microsoft\SystemCertificates\$StoreName\Certificates\$ThumbprintHex"
  if (-not (Test-Path -LiteralPath $certKeyPath)) {
    throw "Certificate not found after injection: $certKeyPath"
  }

  $blob = $null
  try {
    $blob = (Get-ItemProperty -LiteralPath $certKeyPath -Name Blob -ErrorAction Stop).Blob
  } catch {
    throw "Certificate registry entry missing expected 'Blob' value: $certKeyPath"
  }

  if ($null -eq $blob -or $blob.Length -eq 0) {
    throw "Certificate registry entry has an empty 'Blob' value: $certKeyPath"
  }
}

function Patch-MountedImage {
  param(
    [Parameter(Mandatory = $true)]
    [string]$MountDir,
    [Parameter(Mandatory = $true)]
    [byte[][]]$CertificateDers,
    [Parameter(Mandatory = $true)]
    [string[]]$CertStores,
    [Parameter(Mandatory = $true)]
    [bool]$PatchBcdTemplate,
    [Parameter(Mandatory = $true)]
    [bool]$EnableNoIntegrityChecks
  )

  $softwareHivePath = Join-Path $MountDir 'Windows\System32\Config\SOFTWARE'
  if (-not (Test-Path -LiteralPath $softwareHivePath)) {
    throw "Offline SOFTWARE hive not found in mounted image: $softwareHivePath"
  }

  $hiveName = "AERO_OFFLINE_SOFTWARE_$([Guid]::NewGuid().ToString('N'))"
  $hiveLoaded = $false
  try {
    Write-Log "Loading offline SOFTWARE hive: $softwareHivePath -> HKLM\\$hiveName"
    Load-OfflineHive -HivePath $softwareHivePath -HiveName $hiveName
    $hiveLoaded = $true

    $storesText = ($CertStores -join ', ')
    foreach ($certDer in $CertificateDers) {
      $thumbprint = Get-CertificateThumbprintHex -CertificateDer $certDer
      Write-Log ("Injecting certificate into offline store(s): $storesText (thumbprint=$thumbprint)")
      Inject-CertIntoOfflineSoftwareHive -HiveName $hiveName -CertificateDer $certDer -Stores $CertStores
      foreach ($store in $CertStores) {
        Assert-CertPresentInOfflineSoftwareHive -HiveName $hiveName -StoreName $store -ThumbprintHex $thumbprint
      }
    }
  } finally {
    if ($hiveLoaded) {
      Write-Log "Unloading offline SOFTWARE hive: HKLM\\$hiveName"
      Unload-OfflineHive -HiveName $hiveName
    }
  }

  if ($PatchBcdTemplate) {
    $bcdTemplatePath = Join-Path $MountDir 'Windows\System32\Config\BCD-Template'
    if (-not (Test-Path -LiteralPath $bcdTemplatePath)) {
      throw "BCD-Template not found in mounted image: $bcdTemplatePath"
    }
    Write-Log "Patching BCD-Template: $bcdTemplatePath"
    Patch-BcdStore -StorePath $bcdTemplatePath -EnableNoIntegrityChecks $EnableNoIntegrityChecks
  }
}

Assert-Admin
Assert-ToolExists -ToolName dism.exe
Assert-ToolExists -ToolName bcdedit.exe
Assert-ToolExists -ToolName reg.exe

$isoRootFull = (Resolve-Path -LiteralPath $IsoRoot).Path
$certFull = (Resolve-Path -LiteralPath $CertPath).Path

Write-Log "ISO root: $isoRootFull"
Write-Log "Certificate: $certFull"
Write-Log "EnableNoIntegrityChecks: $EnableNoIntegrityChecks"

$normalizedCertStores = Normalize-CertStoreList -Stores $CertStores
Write-Log ("CertStores: " + ($normalizedCertStores -join ', '))

$bootWimPath = Join-Path $isoRootFull 'sources\boot.wim'
$installWimPath = Join-Path $isoRootFull 'sources\install.wim'
$biosBcdPath = Join-Path $isoRootFull 'boot\BCD'
$uefiBcdPath = Join-Path $isoRootFull 'efi\microsoft\boot\BCD'

foreach ($p in @($bootWimPath, $installWimPath, $biosBcdPath, $uefiBcdPath)) {
  if (-not (Test-Path -LiteralPath $p)) {
    throw "Expected path not found: $p"
  }
}

Ensure-FileWritable -Path $bootWimPath
Ensure-FileWritable -Path $installWimPath

$certificateDers = @(Get-CertificateDerBlobs -Path $certFull -PfxPassword $PfxPassword)
if ($certificateDers.Count -eq 0) {
  throw "No certificates loaded from: $certFull"
}

$availableBootIndices = Get-WimIndices -WimFile $bootWimPath
foreach ($idx in $PatchBootWimIndices) {
  if ($availableBootIndices -notcontains $idx) {
    throw "boot.wim index $idx does not exist. Available indices: $($availableBootIndices -join ', ')"
  }
}

$availableInstallIndices = Get-WimIndices -WimFile $installWimPath

$mountRoot = Join-Path $env:TEMP ("aero-win7-media-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $mountRoot -Force | Out-Null

try {
  if ($PatchBootWimIndices.Count -gt 0) {
    foreach ($idx in $PatchBootWimIndices) {
      $mountDir = Join-Path $mountRoot ("bootwim-index-" + $idx)
      $mounted = $false
      $commit = $false
      try {
        Write-Log "Mounting boot.wim index $idx -> $mountDir"
        Mount-Wim -WimFile $bootWimPath -Index $idx -MountDir $mountDir
        $mounted = $true

        Patch-MountedImage -MountDir $mountDir -CertificateDers $certificateDers -CertStores $normalizedCertStores -PatchBcdTemplate:$false -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
        $commit = $true
      } finally {
        if ($mounted) {
          Write-Log "Unmounting boot.wim index $idx (Commit=$commit)"
          Unmount-Wim -MountDir $mountDir -Commit:$commit
        }
      }
    }
  } else {
    Write-Log 'Skipping boot.wim patching (no indices specified)' 'WARN'
  }

  $installIndices = if ($PatchInstallWimAllIndices) {
    Write-Log 'Discovering install.wim indices (PatchInstallWimAllIndices=true)'
    $availableInstallIndices
  } else {
    if (-not $PatchInstallWimIndices -or $PatchInstallWimIndices.Count -eq 0) {
      throw '-PatchInstallWimAllIndices is false but -PatchInstallWimIndices was not provided.'
    }
    $PatchInstallWimIndices
  }

  foreach ($idx in $installIndices) {
    if ($availableInstallIndices -notcontains $idx) {
      throw "install.wim index $idx does not exist. Available indices: $($availableInstallIndices -join ', ')"
    }
  }

  foreach ($idx in $installIndices) {
    $mountDir = Join-Path $mountRoot ("installwim-index-" + $idx)
    $mounted = $false
    $commit = $false
    try {
      Write-Log "Mounting install.wim index $idx -> $mountDir"
      Mount-Wim -WimFile $installWimPath -Index $idx -MountDir $mountDir
      $mounted = $true

      Patch-MountedImage -MountDir $mountDir -CertificateDers $certificateDers -CertStores $normalizedCertStores -PatchBcdTemplate:$true -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
      $commit = $true
    } finally {
      if ($mounted) {
        Write-Log "Unmounting install.wim index $idx (Commit=$commit)"
        Unmount-Wim -MountDir $mountDir -Commit:$commit
      }
    }
  }

  Write-Log "Patching ISO BCD store (BIOS/CSM): $biosBcdPath"
  Patch-BcdStore -StorePath $biosBcdPath -EnableNoIntegrityChecks:$EnableNoIntegrityChecks

  Write-Log "Patching ISO BCD store (UEFI): $uefiBcdPath"
  Patch-BcdStore -StorePath $uefiBcdPath -EnableNoIntegrityChecks:$EnableNoIntegrityChecks

  Write-Log 'Media patching complete.'
  Write-Log 'Suggested verification commands:'
  Write-Host "  bcdedit /store `"$biosBcdPath`" /enum {default}"
  Write-Host "  bcdedit /store `"$uefiBcdPath`" /enum {default}"
} finally {
  try {
    Remove-Item -LiteralPath $mountRoot -Recurse -Force -ErrorAction Stop
  } catch {
    Write-Log "Failed to remove temp root dir (ok to ignore): $mountRoot`n$($_.Exception.Message)" 'WARN'
  }
}
