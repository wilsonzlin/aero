param(
  [Parameter(Mandatory=$true)]
  [string]$OutDir,

  [string]$Subject = "CN=Aero Virtio Test Certificate",

  [int]$ValidYears = 10
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$cert = New-SelfSignedCertificate `
  -Type CodeSigningCert `
  -Subject $Subject `
  -KeyAlgorithm RSA `
  -KeyLength 2048 `
  -HashAlgorithm SHA256 `
  -KeyExportPolicy Exportable `
  -NotAfter (Get-Date).AddYears($ValidYears) `
  -CertStoreLocation "Cert:\CurrentUser\My"

$pfxPath = Join-Path $OutDir "aero-virtio-test.pfx"
$cerPath = Join-Path $OutDir "aero-virtio-test.cer"

$password = Read-Host -Prompt "PFX password (will not echo)" -AsSecureString

Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $password | Out-Null
Export-Certificate -Cert $cert -FilePath $cerPath | Out-Null

Write-Host "Wrote:"
Write-Host "  $pfxPath"
Write-Host "  $cerPath"

