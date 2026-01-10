<#
.SYNOPSIS
  Installs the Aero virtio-input test-signing certificate on a Windows test machine.

.DESCRIPTION
  Adds the certificate (.cer) to the LocalMachine trust stores required for driver installation:
    - Trusted Root Certification Authorities (Root)
    - Trusted Publishers (TrustedPublisher)

  Run from an elevated PowerShell prompt.
#>

[CmdletBinding()]
param(
  [string]$CertPath = (Join-Path $PSScriptRoot "..\\cert\\aero-virtio-input-test.cer")
)

$ErrorActionPreference = "Stop"

$CertPath = (Resolve-Path -LiteralPath $CertPath).Path

Write-Host "Installing test certificate:"
Write-Host "  $CertPath"
Write-Host ""
Write-Host "Target stores:"
Write-Host "  LocalMachine\\Root"
Write-Host "  LocalMachine\\TrustedPublisher"
Write-Host ""

& certutil.exe -addstore -f Root $CertPath | Out-Null
& certutil.exe -addstore -f TrustedPublisher $CertPath | Out-Null

Write-Host "OK: Certificate installed."

