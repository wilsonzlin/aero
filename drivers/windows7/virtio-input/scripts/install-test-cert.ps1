# SPDX-License-Identifier: MIT OR Apache-2.0
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
Set-StrictMode -Version 2.0

function Assert-Elevated {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  $p = New-Object Security.Principal.WindowsPrincipal($id)
  if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated PowerShell prompt (Run as Administrator)."
  }
}

$certutil = Get-Command certutil.exe -ErrorAction SilentlyContinue
if (-not $certutil) {
  throw "certutil.exe not found in PATH."
}

Assert-Elevated

$CertPath = (Resolve-Path -LiteralPath $CertPath).Path

Write-Host "Installing test certificate:"
Write-Host "  $CertPath"
Write-Host ""
Write-Host "Target stores:"
Write-Host "  LocalMachine\\Root"
Write-Host "  LocalMachine\\TrustedPublisher"
Write-Host ""

& certutil.exe -addstore -f Root $CertPath | Out-Null
if ($LASTEXITCODE -ne 0) { throw "certutil.exe -addstore Root failed with exit code $LASTEXITCODE" }

& certutil.exe -addstore -f TrustedPublisher $CertPath | Out-Null
if ($LASTEXITCODE -ne 0) { throw "certutil.exe -addstore TrustedPublisher failed with exit code $LASTEXITCODE" }

Write-Host "OK: Certificate installed."

