# SPDX-License-Identifier: MIT OR Apache-2.0
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
  - Prefers New-SelfSignedCertificate when available (Windows 10/11).
  - Falls back to makecert.exe (older SDK/WDK tool) if New-SelfSignedCertificate cannot create the
    requested certificate (or is not present).
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
Set-StrictMode -Version 2.0

$certutil = Get-Command certutil.exe -ErrorAction SilentlyContinue
if (-not $certutil) {
  throw "certutil.exe not found in PATH."
}

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

function Try-NewSelfSignedCertificate([ValidateSet('sha1', 'sha256')] [string]$HashValue) {
  if (-not $newSelfSigned) { return $null }

  $hashName = Get-HashAlgorithmName $HashValue
  $notAfter = (Get-Date).AddYears(10)

  $cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -KeyAlgorithm RSA `
    -KeyLength 2048 `
    -KeyExportPolicy Exportable `
    -KeySpec Signature `
    -HashAlgorithm $hashName `
    -NotAfter $notAfter `
    -CertStoreLocation "Cert:\\CurrentUser\\My"

  if (-not (Test-CertificateSignatureHash -Cert $cert -ExpectedHash $HashValue)) {
    $actual = "{0} ({1})" -f $cert.SignatureAlgorithm.FriendlyName, $cert.SignatureAlgorithm.Value
    throw "New-SelfSignedCertificate did not produce a $HashValue-signed certificate (got: $actual)."
  }

  return $cert.Thumbprint
}

function Try-MakeCert([ValidateSet('sha1', 'sha256')] [string]$HashValue) {
  if (-not $makecert) { return $null }

  $start = Get-Date
  $end = $start.AddYears(10)

  $startStr = $start.ToString("MM/dd/yyyy")
  $endStr = $end.ToString("MM/dd/yyyy")

  if (Test-Path -LiteralPath $cerPath) { Remove-Item -LiteralPath $cerPath -Force }

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

  if (-not (Test-Path -LiteralPath $cerPath)) {
    throw "makecert.exe did not produce the expected output file: $cerPath"
  }

  $created = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($cerPath)

  if (-not (Test-CertificateSignatureHash -Cert $created -ExpectedHash $HashValue)) {
    $actual = "{0} ({1})" -f $created.SignatureAlgorithm.FriendlyName, $created.SignatureAlgorithm.Value
    throw "makecert.exe produced a certificate with unexpected signature algorithm (expected $HashValue, got: $actual)."
  }

  return $created.Thumbprint
}

$requested = $CertHashAlgorithm.ToLowerInvariant()
$thumbprint = $null
$lastError = $null

foreach ($hashAttempt in @($requested)) {
  try {
    $thumbprint = Try-NewSelfSignedCertificate -HashValue $hashAttempt
    if ($thumbprint) { break }
  }
  catch {
    $lastError = $_
  }

  try {
    $thumbprint = Try-MakeCert -HashValue $hashAttempt
    if ($thumbprint) { break }
  }
  catch {
    $lastError = $_
  }
}

if ((-not $thumbprint) -and ($requested -eq 'sha1') -and $AllowSha2CertFallback) {
  Write-Warning "Failed to create a SHA-1-signed certificate; falling back to SHA-256 due to -AllowSha2CertFallback."
  Write-Warning "WARNING: Stock Windows 7 SP1 without SHA-2 updates (KB3033929 / KB4474419) may reject the signature chain."

  foreach ($hashAttempt in @('sha256')) {
    try {
      $thumbprint = Try-NewSelfSignedCertificate -HashValue $hashAttempt
      if ($thumbprint) { break }
    }
    catch {
      $lastError = $_
    }

    try {
      $thumbprint = Try-MakeCert -HashValue $hashAttempt
      if ($thumbprint) { break }
    }
    catch {
      $lastError = $_
    }
  }
}

if (-not $thumbprint) {
  if ($lastError) { throw $lastError }
  throw "Neither New-SelfSignedCertificate nor makecert.exe is available. Install the Windows SDK/WDK (or run on Windows 10+)."
}

Write-Host "Exporting .cer and .pfx to: $OutDir"

if (Test-Path -LiteralPath $cerPath) { Remove-Item -LiteralPath $cerPath -Force }
if (Test-Path -LiteralPath $pfxPath) { Remove-Item -LiteralPath $pfxPath -Force }

& certutil.exe -user -exportcert -f My $thumbprint $cerPath | Out-Null
if ($LASTEXITCODE -ne 0) { throw "certutil.exe -exportcert failed with exit code $LASTEXITCODE" }
& certutil.exe -user -exportPFX -p $PfxPassword -f My $thumbprint $pfxPath | Out-Null
if ($LASTEXITCODE -ne 0) { throw "certutil.exe -exportPFX failed with exit code $LASTEXITCODE" }

if (-not (Test-Path -LiteralPath $cerPath)) {
  throw "Expected output file not found: $cerPath"
}
if (-not (Test-Path -LiteralPath $pfxPath)) {
  throw "Expected output file not found: $pfxPath"
}

Write-Host ""
Write-Host "Wrote:"
Write-Host "  $cerPath"
Write-Host "  $pfxPath"
Write-Host ""
Write-Host "Next:"
Write-Host "  1) Install the .cer on the Windows 7 test machine (scripts\\install-test-cert.ps1)"
Write-Host "  2) Generate a catalog (scripts\\make-cat.cmd)"
Write-Host "  3) Sign the package (scripts\\sign-driver.cmd)"

