<#
.SYNOPSIS
  Creates a self-signed code-signing certificate for test-signing the Aero virtio-input driver.

.DESCRIPTION
  Generates an exportable code-signing cert in the CurrentUser\My store and exports:
    - aero-virtio-input-test.cer
    - aero-virtio-input-test.pfx

  The exported .cer is installed on the Windows 7 test machine (Trusted Root + Trusted Publishers).
  The exported .pfx is used by signtool to sign the .sys and .cat.

.NOTES
  - Uses New-SelfSignedCertificate when available (Windows 10/11).
  - Falls back to makecert.exe (older SDK/WDK tool) if New-SelfSignedCertificate is not present.
  - Export is done via certutil.exe for broad Windows compatibility.
  - Defaults to a SHA-1-signed certificate for maximum Windows 7 compatibility. A SHA-2-signed
    certificate may not be accepted by stock Windows 7 SP1 without SHA-2 updates
    (KB3033929 / KB4474419), even if the driver file digest uses SHA-1.
#>

[CmdletBinding()]
param(
  [string]$OutDir = (Join-Path $PSScriptRoot "..\\cert"),
  [string]$Subject = "CN=Aero virtio-input Test Certificate",
  [string]$PfxPassword,
  [ValidateSet('sha1', 'sha256')]
  [string]$CertHashAlgorithm = 'sha1',
  [switch]$AllowSha2CertFallback
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $OutDir)) {
  New-Item -ItemType Directory -Path $OutDir | Out-Null
}
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path

$cerPath = Join-Path $OutDir "aero-virtio-input-test.cer"
$pfxPath = Join-Path $OutDir "aero-virtio-input-test.pfx"

if (-not $PfxPassword) {
  $secure = Read-Host -AsSecureString "Enter password to protect the exported PFX"
  $PfxPassword = (New-Object System.Net.NetworkCredential("", $secure)).Password
}
if ([string]::IsNullOrEmpty($PfxPassword)) {
  throw "PfxPassword cannot be empty."
}

Write-Host "Creating test code-signing certificate in CurrentUser\\My..."

$thumbprint = $null

$newSelfSigned = Get-Command New-SelfSignedCertificate -ErrorAction SilentlyContinue
$makecert = Get-Command makecert.exe -ErrorAction SilentlyContinue

function Test-CertificateSignatureHash {
  param(
    [Parameter(Mandatory = $true)]
    $Cert,

    [Parameter(Mandatory = $true)]
    [ValidateSet('sha1', 'sha256')]
    [string]$ExpectedHash
  )

  $expectedOids = @{
    sha1   = @('1.2.840.113549.1.1.5', '1.3.14.3.2.29', '1.2.840.10045.4.1')
    sha256 = @('1.2.840.113549.1.1.11', '1.2.840.10045.4.3.2')
  }

  $oid = [string]$Cert.SignatureAlgorithm.Value
  if ($expectedOids[$ExpectedHash] -contains $oid) {
    return $true
  }

  $friendly = [string]$Cert.SignatureAlgorithm.FriendlyName
  if (-not [string]::IsNullOrWhiteSpace($friendly)) {
    if ($friendly.ToLowerInvariant().Contains($ExpectedHash.ToLowerInvariant())) {
      return $true
    }
  }

  return $false
}

function Get-HashAlgorithmName([ValidateSet('sha1', 'sha256')] [string]$HashValue) {
  if ($HashValue -eq 'sha1') { return 'SHA1' }
  return 'SHA256'
}

if ($newSelfSigned) {
  $requested = $CertHashAlgorithm.ToLowerInvariant()
  $hashName = Get-HashAlgorithmName $requested

  try {
    $cert = New-SelfSignedCertificate `
      -Type CodeSigningCert `
      -Subject $Subject `
      -KeyAlgorithm RSA `
      -KeyLength 2048 `
      -KeyExportPolicy Exportable `
      -HashAlgorithm $hashName `
      -CertStoreLocation "Cert:\\CurrentUser\\My"

    if (-not (Test-CertificateSignatureHash -Cert $cert -ExpectedHash $requested)) {
      $actual = "{0} ({1})" -f $cert.SignatureAlgorithm.FriendlyName, $cert.SignatureAlgorithm.Value
      throw "New-SelfSignedCertificate did not produce a $requested-signed certificate (got: $actual)."
    }

    $thumbprint = $cert.Thumbprint
  }
  catch {
    if (($requested -eq 'sha1') -and $AllowSha2CertFallback) {
      Write-Warning "Failed to create a SHA-1-signed certificate; falling back to SHA-256 due to -AllowSha2CertFallback."
      Write-Warning "WARNING: Stock Windows 7 SP1 without SHA-2 updates (KB3033929 / KB4474419) may reject the signature chain."

      $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $Subject `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeyExportPolicy Exportable `
        -HashAlgorithm (Get-HashAlgorithmName 'sha256') `
        -CertStoreLocation "Cert:\\CurrentUser\\My"

      if (-not (Test-CertificateSignatureHash -Cert $cert -ExpectedHash 'sha256')) {
        $actual = "{0} ({1})" -f $cert.SignatureAlgorithm.FriendlyName, $cert.SignatureAlgorithm.Value
        throw "SHA-256 fallback certificate had unexpected signature algorithm (got: $actual)."
      }

      $thumbprint = $cert.Thumbprint
    }
    else {
      throw
    }
  }
}
elseif ($makecert) {
  $start = Get-Date
  $end = $start.AddYears(10)

  $startStr = $start.ToString("MM/dd/yyyy")
  $endStr = $end.ToString("MM/dd/yyyy")

  $requested = $CertHashAlgorithm.ToLowerInvariant()

  function Invoke-MakeCert([ValidateSet('sha1', 'sha256')] [string]$HashValue) {
    & makecert.exe `
      -r `
      -pe `
      -ss My `
      -sr CurrentUser `
      -n $Subject `
      -eku 1.3.6.1.5.5.7.3.3 `
      -a $HashValue `
      -len 2048 `
      -b $startStr `
      -e $endStr `
      $cerPath | Out-Null
  }

  try {
    Invoke-MakeCert -HashValue $requested
  }
  catch {
    if (($requested -eq 'sha1') -and $AllowSha2CertFallback) {
      Write-Warning "makecert.exe failed to create a SHA-1-signed certificate; falling back to SHA-256 due to -AllowSha2CertFallback."
      Write-Warning "WARNING: Stock Windows 7 SP1 without SHA-2 updates (KB3033929 / KB4474419) may reject the signature chain."
      Invoke-MakeCert -HashValue 'sha256'
      $requested = 'sha256'
    }
    else {
      throw
    }
  }

  $store = New-Object System.Security.Cryptography.X509Certificates.X509Store("My", "CurrentUser")
  $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
  $match = $store.Certificates | Where-Object { $_.Subject -eq $Subject } | Sort-Object NotBefore -Descending | Select-Object -First 1
  $store.Close()

  if (-not $match) {
    throw "makecert.exe succeeded but the certificate could not be located in CurrentUser\\My."
  }

  if (-not (Test-CertificateSignatureHash -Cert $match -ExpectedHash $requested)) {
    $actual = "{0} ({1})" -f $match.SignatureAlgorithm.FriendlyName, $match.SignatureAlgorithm.Value
    throw "makecert.exe produced a certificate with unexpected signature algorithm (expected $requested, got: $actual)."
  }

  $thumbprint = $match.Thumbprint
}
else {
  throw "Neither New-SelfSignedCertificate nor makecert.exe is available. Install the Windows SDK/WDK (or run on Windows 10+)."
}

if (-not $thumbprint) {
  throw "Failed to determine certificate thumbprint."
}

Write-Host "Exporting .cer and .pfx to: $OutDir"

if (Test-Path -LiteralPath $cerPath) { Remove-Item -LiteralPath $cerPath -Force }
if (Test-Path -LiteralPath $pfxPath) { Remove-Item -LiteralPath $pfxPath -Force }

& certutil.exe -user -exportcert -f My $thumbprint $cerPath | Out-Null
& certutil.exe -user -exportPFX -p $PfxPassword -f My $thumbprint $pfxPath | Out-Null

Write-Host ""
Write-Host "Wrote:"
Write-Host "  $cerPath"
Write-Host "  $pfxPath"
Write-Host ""
Write-Host "Next:"
Write-Host "  1) Install the .cer on the Windows 7 test machine (scripts\\install-test-cert.ps1)"
Write-Host "  2) Generate a catalog (scripts\\make-cat.cmd)"
Write-Host "  3) Sign the package (scripts\\sign-driver.cmd)"

