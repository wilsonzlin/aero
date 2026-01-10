[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$OutDir,

  [string]$Subject = "CN=Aero Virtio Test Certificate",

  [int]$ValidYears = 10,

  # For maximum Windows 7 out-of-box compatibility (no SHA-2 updates), use SHA-1
  # for both the file digest (/fd SHA1) and the certificate signature algorithm.
  [ValidateSet("sha1", "sha256")]
  [string]$CertHashAlgorithm = "sha1",

  # If SHA-1 certificate creation fails on this machine, require explicit opt-in
  # to fall back to a SHA-256-signed certificate (which may fail on stock Win7
  # without KB3033929/KB4474419 even if /fd SHA1 is used).
  [switch]$AllowSha2CertFallback
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

try {
  Import-Module -Name PKI -ErrorAction Stop
} catch {
  # PowerShell 7 often needs WindowsPowerShell-compat import for inbox modules like PKI.
  if ($PSVersionTable.PSVersion.Major -ge 7) {
    Import-Module -Name PKI -UseWindowsPowerShell -ErrorAction SilentlyContinue
  }
}

function Test-CertificateSignatureHash {
  param(
    [Parameter(Mandatory = $true)]
    $Cert,

    [Parameter(Mandatory = $true)]
    [ValidateSet("sha1", "sha256")]
    [string]$ExpectedHash
  )

  $expectedOids = @{
    sha1   = @("1.2.840.113549.1.1.5", "1.3.14.3.2.29", "1.2.840.10045.4.1")
    sha256 = @("1.2.840.113549.1.1.11", "1.2.840.10045.4.3.2")
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

function Get-HashAlgorithmName([ValidateSet("sha1", "sha256")] [string]$HashValue) {
  if ($HashValue -eq "sha1") { return "SHA1" }
  return "SHA256"
}

function Write-CertificateInfo {
  param(
    [Parameter(Mandatory = $true)]
    $Cert
  )

  Write-Host "Certificate:"
  Write-Host "  Subject:            $($Cert.Subject)"
  Write-Host "  Thumbprint:         $($Cert.Thumbprint)"
  Write-Host "  SignatureAlgorithm: $($Cert.SignatureAlgorithm.FriendlyName) ($($Cert.SignatureAlgorithm.Value))"
  Write-Host "  NotAfter:           $($Cert.NotAfter.ToString('u'))"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$requested = $CertHashAlgorithm.ToLowerInvariant()
$hashName = Get-HashAlgorithmName $requested
$notAfter = (Get-Date).AddYears($ValidYears)

$cert = $null
try {
  $cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -KeyAlgorithm RSA `
    -KeyLength 2048 `
    -HashAlgorithm $hashName `
    -KeyExportPolicy Exportable `
    -NotAfter $notAfter `
    -CertStoreLocation "Cert:\CurrentUser\My"

  if (-not (Test-CertificateSignatureHash -Cert $cert -ExpectedHash $requested)) {
    $actual = "{0} ({1})" -f $cert.SignatureAlgorithm.FriendlyName, $cert.SignatureAlgorithm.Value
    throw "New-SelfSignedCertificate did not produce a $requested-signed certificate (got: $actual)."
  }
} catch {
  if (($requested -ne "sha1") -or (-not $AllowSha2CertFallback)) {
    throw
  }

  Write-Warning "Requested a SHA-1-signed certificate (-HashAlgorithm SHA1) but certificate creation failed."
  Write-Warning "Proceeding due to -AllowSha2CertFallback: creating a SHA-256-signed certificate instead."
  Write-Warning "WARNING: Stock Windows 7 SP1 without KB3033929/KB4474419 may fail to validate the signature chain, even if /fd SHA1 is used."

  $cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -KeyAlgorithm RSA `
    -KeyLength 2048 `
    -HashAlgorithm (Get-HashAlgorithmName "sha256") `
    -KeyExportPolicy Exportable `
    -NotAfter $notAfter `
    -CertStoreLocation "Cert:\CurrentUser\My"

  if (-not (Test-CertificateSignatureHash -Cert $cert -ExpectedHash "sha256")) {
    $actual = "{0} ({1})" -f $cert.SignatureAlgorithm.FriendlyName, $cert.SignatureAlgorithm.Value
    throw "SHA-256 fallback certificate had unexpected signature algorithm (got: $actual)."
  }
}

Write-CertificateInfo -Cert $cert

$pfxPath = Join-Path $OutDir "aero-virtio-test.pfx"
$cerPath = Join-Path $OutDir "aero-virtio-test.cer"

$password = Read-Host -Prompt "PFX password (will not echo)" -AsSecureString

Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $password | Out-Null
Export-Certificate -Cert $cert -FilePath $cerPath | Out-Null

Write-Host "Wrote:"
Write-Host "  $pfxPath"
Write-Host "  $cerPath"

