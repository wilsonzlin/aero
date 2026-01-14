#Requires -Version 5.1
# SPDX-License-Identifier: MIT OR Apache-2.0

<#
.SYNOPSIS
Deprecated compatibility wrapper for inject-driver.ps1 (offline directory mode).

.DESCRIPTION
This repository previously shipped Inject-VirtioInputDriverOffline.ps1 as a helper script for
staging the virtio-input driver into an already-installed offline Windows directory.

Use inject-driver.ps1 (or inject-driver.cmd) instead. It supports both WIM mode and offline
directory mode and provides a single canonical workflow.

This wrapper is kept for backwards compatibility and forwards to inject-driver.ps1, mapping:
  -ImagePath        -> -OfflineDir
  -DriverPackageDir -> -DriverDir

.PARAMETER ImagePath
Path to the offline Windows root directory (must contain a 'Windows\' directory), e.g. W:\.

.PARAMETER DriverPackageDir
Directory containing aero_virtio_input.inf and aero_virtio_input.sys (and optionally aero_virtio_input.cat).

.PARAMETER ForceUnsigned
Pass /ForceUnsigned to DISM /Add-Driver (test-only).

.PARAMETER Help
Show usage for the canonical script.
#>

[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
  [Parameter(ParameterSetName = 'Help')]
  [Alias('?')]
  [switch]$Help,

  [Parameter(Mandatory, ParameterSetName = 'Run')]
  [ValidateNotNullOrEmpty()]
  [string]$ImagePath,

  [Parameter(Mandatory, ParameterSetName = 'Run')]
  [ValidateNotNullOrEmpty()]
  [string]$DriverPackageDir,

  [Parameter(ParameterSetName = 'Run')]
  [switch]$ForceUnsigned
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$injectScript = Join-Path -Path $PSScriptRoot -ChildPath "inject-driver.ps1"
if (-not (Test-Path -LiteralPath $injectScript -PathType Leaf)) {
  throw "inject-driver.ps1 was not found next to this script: $injectScript"
}

Write-Warning "DEPRECATED: Inject-VirtioInputDriverOffline.ps1 has been replaced by inject-driver.ps1 / inject-driver.cmd."
Write-Warning "This wrapper is kept for backwards compatibility and may be removed in a future cleanup."

if ($Help) {
  # Delegate help text to the canonical script so there is a single source of truth.
  & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $injectScript -Help
  exit $LASTEXITCODE
}

$forwardArgs = @(
  "-OfflineDir", $ImagePath,
  "-DriverDir", $DriverPackageDir
)
if ($ForceUnsigned) {
  $forwardArgs += "-ForceUnsigned"
}

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $injectScript @forwardArgs
exit $LASTEXITCODE

