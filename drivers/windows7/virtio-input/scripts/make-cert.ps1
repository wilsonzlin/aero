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
#>

[CmdletBinding()]
param(
  [string]$OutDir = (Join-Path $PSScriptRoot "..\\cert"),
  [string]$Subject = "CN=Aero virtio-input Test Certificate",
  [string]$PfxPassword
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

if ($newSelfSigned) {
  $cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -KeyAlgorithm RSA `
    -KeyLength 2048 `
    -KeyExportPolicy Exportable `
    -HashAlgorithm SHA256 `
    -CertStoreLocation "Cert:\\CurrentUser\\My"

  $thumbprint = $cert.Thumbprint
}
elseif ($makecert) {
  $start = Get-Date
  $end = $start.AddYears(10)

  $startStr = $start.ToString("MM/dd/yyyy")
  $endStr = $end.ToString("MM/dd/yyyy")

  & makecert.exe `
    -r `
    -pe `
    -ss My `
    -sr CurrentUser `
    -n $Subject `
    -eku 1.3.6.1.5.5.7.3.3 `
    -a sha256 `
    -len 2048 `
    -b $startStr `
    -e $endStr `
    $cerPath | Out-Null

  $store = New-Object System.Security.Cryptography.X509Certificates.X509Store("My", "CurrentUser")
  $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
  $match = $store.Certificates | Where-Object { $_.Subject -eq $Subject } | Sort-Object NotBefore -Descending | Select-Object -First 1
  $store.Close()

  if (-not $match) {
    throw "makecert.exe succeeded but the certificate could not be located in CurrentUser\\My."
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

