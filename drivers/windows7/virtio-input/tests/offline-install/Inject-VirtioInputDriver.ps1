#Requires -Version 5.1

<#
.SYNOPSIS
Deprecated compatibility wrapper for inject-driver.ps1.

.DESCRIPTION
This repository previously shipped Inject-VirtioInputDriver.ps1 as the primary automation script
for WIM-based driver injection.

Use inject-driver.ps1 (or inject-driver.cmd) instead. It supports both:
  - WIM mode (mount -> add-driver -> verify -> unmount)
  - Offline directory mode (add-driver -> verify)

This wrapper is kept for backwards compatibility and forwards to inject-driver.ps1, mapping:
  -DriverPackageDir -> -DriverDir

.PARAMETER WimPath
Path to install.wim or boot.wim.

.PARAMETER Index
Image index inside the WIM.

.PARAMETER DriverPackageDir
Directory containing aero_virtio_input.inf/.sys (and optionally .cat).

.PARAMETER MountDir
Optional mount directory.

.PARAMETER ForceUnsigned
Pass /ForceUnsigned to DISM /Add-Driver.

.PARAMETER Commit
Whether to commit the WIM modifications on unmount (default: commit).
Use -Commit:$false to discard changes after verification (dry run).

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
  [string]$WimPath,

  [Parameter(Mandatory, ParameterSetName = 'Run')]
  [ValidateRange(1, [int]::MaxValue)]
  [int]$Index,

  [Parameter(Mandatory, ParameterSetName = 'Run')]
  [ValidateNotNullOrEmpty()]
  [string]$DriverPackageDir,

  [Parameter(ParameterSetName = 'Run')]
  [string]$MountDir = "",

  [Parameter(ParameterSetName = 'Run')]
  [switch]$ForceUnsigned,

  [Parameter(ParameterSetName = 'Run')]
  [switch]$Commit = $true
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$injectScript = Join-Path -Path $PSScriptRoot -ChildPath "inject-driver.ps1"
if (-not (Test-Path -LiteralPath $injectScript -PathType Leaf)) {
  throw "inject-driver.ps1 was not found next to this script: $injectScript"
}

Write-Warning "DEPRECATED: Inject-VirtioInputDriver.ps1 has been replaced by inject-driver.ps1 / inject-driver.cmd."
Write-Warning "This wrapper is kept for backwards compatibility and may be removed in a future cleanup."

if ($Help) {
  # Delegate help text to the canonical script so there is a single source of truth.
  & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $injectScript -Help
  exit $LASTEXITCODE
}

# Preserve the old script's behavior of clearing the read-only attribute when possible.
# This helps when install media extract tools mark WIMs read-only.
try {
  if (Get-Command attrib.exe -ErrorAction SilentlyContinue) {
    & attrib.exe -r $WimPath 2>$null | Out-Null
  }
} catch {
  # Best-effort only.
}

try {
  $resolvedWimPath = (Resolve-Path -LiteralPath $WimPath -ErrorAction Stop).Path
  $item = Get-Item -LiteralPath $resolvedWimPath -ErrorAction Stop
  if ($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) {
    throw ("WIM file is read-only and cannot be serviced in-place. Copy it to a writable NTFS directory and retry.`n" +
      "Path: $resolvedWimPath")
  }
} catch {
  # If path resolution fails, inject-driver.ps1 will emit its own validation error.
}

$mountDirProvided = -not [string]::IsNullOrWhiteSpace($MountDir)
$resolvedMountDir = $null
if ($mountDirProvided) {
  $resolvedMountDir = [System.IO.Path]::GetFullPath($MountDir)

  if (Test-Path -LiteralPath $resolvedMountDir) {
    if (-not (Test-Path -LiteralPath $resolvedMountDir -PathType Container)) {
      throw "MountDir path exists but is not a directory: $resolvedMountDir"
    }
    $items = @(Get-ChildItem -LiteralPath $resolvedMountDir -Force -ErrorAction SilentlyContinue)
    if ($items.Count -ne 0) {
      throw "MountDir must be empty. Directory is not empty: $resolvedMountDir"
    }
  } else {
    New-Item -ItemType Directory -Path $resolvedMountDir | Out-Null
  }
}

$commitBool = [bool]$Commit

$forwardArgs = @(
  "-WimPath", $WimPath,
  "-Index", "$Index",
  "-DriverDir", $DriverPackageDir,
  "-Commit:$commitBool"
)

if ($mountDirProvided) {
  $forwardArgs += @("-MountDir", $resolvedMountDir)
}
if ($ForceUnsigned) {
  $forwardArgs += "-ForceUnsigned"
}

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $injectScript @forwardArgs
$exitCode = $LASTEXITCODE

if ($mountDirProvided) {
  # Keep the legacy script's behavior of removing the mount dir on completion, but only if it's empty.
  try {
    if (Test-Path -LiteralPath $resolvedMountDir -PathType Container) {
      $items = @(Get-ChildItem -LiteralPath $resolvedMountDir -Force -ErrorAction SilentlyContinue)
      if ($items.Count -eq 0) {
        Remove-Item -LiteralPath $resolvedMountDir -Force -ErrorAction Stop
      }
    }
  } catch {
    Write-Warning "Failed to delete mount directory '$resolvedMountDir'. You can remove it manually after ensuring the image is unmounted."
  }
}

exit $exitCode
