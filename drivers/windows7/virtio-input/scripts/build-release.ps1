# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  One-shot helper to stage, catalog, sign, and package the virtio-input driver for Win7.

.DESCRIPTION
  This script is a convenience wrapper around the per-step scripts:

  - make-cert.ps1
  - stage-built-sys.ps1
  - make-cat.cmd
  - sign-driver.cmd
  - package-release.ps1

  It can run the workflow for a single architecture or for both architectures
  sequentially (x86 then amd64). This matters because the binary name is the same
  (`aero_virtio_input.sys`) on both architectures, and `Inf2Cat` hashes the staged SYS.

  Run this from a WDK Developer Command Prompt so `Inf2Cat.exe` and `signtool.exe`
  are in `PATH`.
#>

[CmdletBinding()]
param(
  [ValidateSet('x86', 'amd64', 'both')]
  [string]$Arch = 'both',

  [ValidateNotNullOrEmpty()]
  [string]$InputDir = (Join-Path $PSScriptRoot '..'),

  [string]$PfxPassword,

  [switch]$AllowSha2CertFallback,

  [ValidateNotNullOrEmpty()]
  [string]$OutDir = (Join-Path $PSScriptRoot '..\\release\\out')
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$driverRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$infDir = Join-Path $driverRoot 'inf'

$makeCert = Join-Path $PSScriptRoot 'make-cert.ps1'
$stageSys = Join-Path $PSScriptRoot 'stage-built-sys.ps1'
$makeCat = Join-Path $PSScriptRoot 'make-cat.cmd'
$signDriver = Join-Path $PSScriptRoot 'sign-driver.cmd'
$packageRelease = Join-Path $PSScriptRoot 'package-release.ps1'

$pfxPath = Join-Path $driverRoot 'cert\\aero-virtio-input-test.pfx'

function Invoke-CheckedCommand([string]$Name, [scriptblock]$Action) {
  Write-Host ""
  Write-Host ("== {0} ==" -f $Name)

  # Avoid reporting stale exit codes from previous native commands (PowerShell
  # scripts do not reliably clear $LASTEXITCODE).
  $global:LASTEXITCODE = 0

  try {
    & $Action
  }
  catch {
    throw ("{0} failed: {1}" -f $Name, $_.Exception.Message)
  }

  if ($LASTEXITCODE -ne 0) {
    throw ("{0} failed (exit code {1})." -f $Name, $LASTEXITCODE)
  }
}

if (-not $PfxPassword) {
  $secure = Read-Host -AsSecureString "Enter PFX password (used for signing; will be passed via environment variable PFX_PASSWORD)"
  $PfxPassword = (New-Object System.Net.NetworkCredential("", $secure)).Password
}
if ([string]::IsNullOrEmpty($PfxPassword)) {
  throw "PfxPassword cannot be empty."
}

if (-not (Test-Path -LiteralPath $pfxPath -PathType Leaf)) {
  Invoke-CheckedCommand "make-cert.ps1" {
    & $makeCert -PfxPassword $PfxPassword -AllowSha2CertFallback:$AllowSha2CertFallback
  }
}

$archList = if ($Arch -eq 'both') { @('x86', 'amd64') } else { @($Arch) }

$oldPfxEnv = $env:PFX_PASSWORD
$env:PFX_PASSWORD = $PfxPassword
try {
  foreach ($a in $archList) {
    Invoke-CheckedCommand ("stage-built-sys.ps1 ({0})" -f $a) {
      & $stageSys -Arch $a -InputDir $InputDir
    }

    Invoke-CheckedCommand "make-cat.cmd" {
      & $makeCat
    }

    Invoke-CheckedCommand "sign-driver.cmd" {
      & $signDriver
    }

    Invoke-CheckedCommand ("package-release.ps1 ({0})" -f $a) {
      & $packageRelease -Arch $a -InputDir $infDir -OutDir $OutDir
    }
  }
}
finally {
  $env:PFX_PASSWORD = $oldPfxEnv
}

Write-Host ""
Write-Host "Done. Output written to:"
Write-Host ("  {0}" -f $OutDir)
Write-Host ""
Write-Host "Next (inside the guest): enable test signing and install the .cer into Root + TrustedPublisher."

