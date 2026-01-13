# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Configure, build, and run the virtio-snd host protocol unit tests.

.DESCRIPTION
  PowerShell equivalent of scripts/run-host-tests.sh.

  Defaults:
    -BuildDir out/virtiosnd-host-tests (relative to the repo root)

  Examples:
    # From the repo root:
    pwsh -NoProfile -ExecutionPolicy Bypass -File drivers/windows7/virtio-snd/scripts/run-host-tests.ps1

    # Clean rebuild:
    pwsh -NoProfile -ExecutionPolicy Bypass -File drivers/windows7/virtio-snd/scripts/run-host-tests.ps1 -Clean

    # Custom build output directory:
    pwsh -NoProfile -ExecutionPolicy Bypass -File drivers/windows7/virtio-snd/scripts/run-host-tests.ps1 -BuildDir out/my-tests
#>

[CmdletBinding()]
param(
  [switch]$Clean,

  [string]$BuildDir,

  # For multi-config generators (Visual Studio), ctest needs a configuration.
  [string]$Configuration = 'Release'
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Invoke-CheckedCommand([string]$Name, [scriptblock]$Action) {
  Write-Host ""
  Write-Host ("== {0} ==" -f $Name)
  # Avoid reporting stale exit codes from previous native commands (PowerShell scripts do not
  # reliably clear $LASTEXITCODE).
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

if (-not (Get-Command -Name cmake -ErrorAction SilentlyContinue)) {
  throw "error: cmake not found in PATH"
}
if (-not (Get-Command -Name ctest -ErrorAction SilentlyContinue)) {
  throw "error: ctest not found in PATH (usually provided by CMake)"
}

$scriptDir = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($scriptDir)) {
  throw "Unable to determine script directory (PSScriptRoot is empty)."
}

$virtioSndDir = (Resolve-Path -LiteralPath (Join-Path $scriptDir '..')).Path
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $virtioSndDir '..\\..\\..')).Path
$srcDir = Join-Path $virtioSndDir 'tests\\host'

if (-not (Test-Path -LiteralPath $srcDir -PathType Container)) {
  throw "virtio-snd host tests directory not found: $srcDir"
}

$resolvedBuildDir = $BuildDir
if ([string]::IsNullOrWhiteSpace($resolvedBuildDir)) {
  $resolvedBuildDir = Join-Path $repoRoot 'out\\virtiosnd-host-tests'
} else {
  if ([System.IO.Path]::IsPathRooted($resolvedBuildDir)) {
    $resolvedBuildDir = [System.IO.Path]::GetFullPath($resolvedBuildDir)
  } else {
    $resolvedBuildDir = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $resolvedBuildDir))
  }
}

if ($Clean) {
  Write-Host ("Cleaning build directory: {0}" -f $resolvedBuildDir)
  if (Test-Path -LiteralPath $resolvedBuildDir) {
    Remove-Item -LiteralPath $resolvedBuildDir -Recurse -Force
  }
}

Write-Host ("Configuring: {0} -> {1}" -f $srcDir, $resolvedBuildDir)
Invoke-CheckedCommand "cmake configure" {
  cmake -S $srcDir -B $resolvedBuildDir
}

$cachePath = Join-Path $resolvedBuildDir 'CMakeCache.txt'
$isMultiConfig = $false
if (Test-Path -LiteralPath $cachePath -PathType Leaf) {
  $isMultiConfig = (Select-String -LiteralPath $cachePath -Pattern '^CMAKE_CONFIGURATION_TYPES:STRING=' -ErrorAction SilentlyContinue | Select-Object -First 1) -ne $null
}

if ($isMultiConfig) {
  if ([string]::IsNullOrWhiteSpace($Configuration)) {
    throw "Configuration cannot be empty for multi-config generators."
  }

  Invoke-CheckedCommand ("cmake build ({0})" -f $Configuration) {
    cmake --build $resolvedBuildDir --config $Configuration
  }

  Invoke-CheckedCommand ("ctest ({0})" -f $Configuration) {
    Push-Location $resolvedBuildDir
    try {
      ctest --output-on-failure -C $Configuration
    }
    finally {
      Pop-Location
    }
  }
} else {
  Invoke-CheckedCommand "cmake build" {
    cmake --build $resolvedBuildDir
  }

  Invoke-CheckedCommand "ctest" {
    Push-Location $resolvedBuildDir
    try {
      ctest --output-on-failure
    }
    finally {
      Pop-Location
    }
  }
}

