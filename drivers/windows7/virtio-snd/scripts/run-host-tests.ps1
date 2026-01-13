# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Configure, build, and run the virtio-snd host-buildable unit tests on Windows.

.DESCRIPTION
  The virtio-snd unit tests live under `drivers/windows7/virtio-snd/tests/`.
  This top-level CMake project is the **superset** and includes:
    - Protocol tests (e.g. test_proto.c)
    - Host shim tests (tests/host/)

  This script is the PowerShell equivalent of `scripts/run-host-tests.sh` and can:
    - Build the full suite (`tests/`) (default)
    - Build only `tests/host/` with `-HostOnly`

  It can be run from anywhere (repo root, driver directory, etc). It locates the repo root
  based on the script location.

  Defaults:
    - (full suite)  out/virtiosnd-tests
    - (-HostOnly)   out/virtiosnd-host-tests

.PARAMETER HostOnly
  Build only `drivers/windows7/virtio-snd/tests/host/` (subset) instead of the
  full suite.

.PARAMETER Clean
  Delete the build directory before configuring.

.PARAMETER BuildDir
  Build directory to use. If relative, it is interpreted relative to the repo root.

.PARAMETER Configuration
  Build/test configuration to use for multi-config generators (Visual Studio,
  Ninja Multi-Config). Defaults to Release.

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File drivers\windows7\virtio-snd\scripts\run-host-tests.ps1

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -BuildDir out\my-virtiosnd-tests

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Clean

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -HostOnly

.EXAMPLE
  pwsh -NoProfile -ExecutionPolicy Bypass -File drivers\windows7\virtio-snd\scripts\run-host-tests.ps1 -Configuration Debug
#>

[CmdletBinding()]
param(
  [switch]$HostOnly,
  [switch]$Clean,
  [string]$BuildDir,
  [string]$Configuration = 'Release'
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Fail([string]$Message, [int]$ExitCode = 1) {
  # Avoid `Write-Error` turning into a terminating error due to
  # `$ErrorActionPreference = 'Stop'` (we want to control the process exit code).
  [Console]::Error.WriteLine($Message)
  exit $ExitCode
}

function Require-Command([string]$Name, [string]$Help) {
  $cmd = Get-Command $Name -ErrorAction SilentlyContinue
  if (-not $cmd) {
    Fail ("error: {0} not found in PATH. {1}" -f $Name, $Help)
  }
  return $cmd.Path
}

function Invoke-CheckedCommand([string]$Name, [string]$Exe, [string[]]$Args) {
  Write-Host ""
  Write-Host ("== {0} ==" -f $Name)
  Write-Host ("{0} {1}" -f $Exe, ($Args -join ' '))
  # Avoid reporting stale exit codes from previous native commands.
  $global:LASTEXITCODE = 0
  & $Exe @Args
  if ($LASTEXITCODE -ne 0) {
    Fail ("{0} failed (exit code {1})." -f $Name, $LASTEXITCODE) $LASTEXITCODE
  }
}

$cmake = Require-Command 'cmake' 'Install CMake and reopen your terminal, then try again.'
$ctest = Require-Command 'ctest' 'ctest is usually installed alongside CMake.'

# Script lives in: drivers/windows7/virtio-snd/scripts/
# Repo root is four levels up from this directory.
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..\..\..')).Path

$testsRoot = Join-Path $repoRoot 'drivers\windows7\virtio-snd\tests'
$srcDir = if ($HostOnly) { Join-Path $testsRoot 'host' } else { $testsRoot }

if (-not (Test-Path -LiteralPath $srcDir -PathType Container)) {
  Fail ("error: virtio-snd tests directory not found: {0}" -f $srcDir)
}

$defaultBuildDir = if ($HostOnly) { 'out/virtiosnd-host-tests' } else { 'out/virtiosnd-tests' }
$resolvedBuildDir = $BuildDir
if ([string]::IsNullOrWhiteSpace($resolvedBuildDir)) {
  $resolvedBuildDir = Join-Path $repoRoot $defaultBuildDir
} elseif ([System.IO.Path]::IsPathRooted($resolvedBuildDir)) {
  $resolvedBuildDir = [System.IO.Path]::GetFullPath($resolvedBuildDir)
} else {
  # Relative paths are interpreted relative to the repo root.
  $resolvedBuildDir = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $resolvedBuildDir))
}

Write-Host ("Repo root : {0}" -f $repoRoot)
Write-Host ("Source dir: {0}" -f $srcDir)
Write-Host ("Build dir : {0}" -f $resolvedBuildDir)

if ($Clean) {
  Write-Host ("Cleaning build directory: {0}" -f $resolvedBuildDir)
  if (Test-Path -LiteralPath $resolvedBuildDir) {
    Remove-Item -LiteralPath $resolvedBuildDir -Recurse -Force
  }
}

# Configure.
# For single-config generators (Ninja/Makefiles), this selects Release by default.
# For multi-config generators (Visual Studio), CMAKE_BUILD_TYPE is ignored.
Invoke-CheckedCommand 'cmake configure' $cmake @(
  '-S', $srcDir,
  '-B', $resolvedBuildDir,
  '-DCMAKE_BUILD_TYPE=Release'
)

# Detect whether the configured generator is multi-config (Visual Studio, Ninja Multi-Config, ...).
$cachePath = Join-Path $resolvedBuildDir 'CMakeCache.txt'
if (-not (Test-Path -LiteralPath $cachePath -PathType Leaf)) {
  Fail ("error: CMake did not produce {0}. Configuration likely failed." -f $cachePath)
}
$cacheText = Get-Content -LiteralPath $cachePath -Raw
$isMultiConfig = [regex]::IsMatch($cacheText, '(?m)^CMAKE_CONFIGURATION_TYPES:STRING=')

$generatorMatch = [regex]::Match($cacheText, '(?m)^CMAKE_GENERATOR:INTERNAL=(.*)$')
if ($generatorMatch.Success) {
  Write-Host ("CMake generator: {0}" -f $generatorMatch.Groups[1].Value)
}
Write-Host ("Multi-config  : {0}" -f $(if ($isMultiConfig) { 'yes' } else { 'no' }))

if ($isMultiConfig -and [string]::IsNullOrWhiteSpace($Configuration)) {
  Fail 'error: -Configuration cannot be empty for multi-config generators.'
}

# Build.
$buildArgs = @('--build', $resolvedBuildDir)
if ($isMultiConfig) {
  $buildArgs += @('--config', $Configuration)
}
Invoke-CheckedCommand 'cmake build' $cmake $buildArgs

# Test.
$testArgs = @('--test-dir', $resolvedBuildDir, '--output-on-failure')
if ($isMultiConfig) {
  $testArgs += @('-C', $Configuration)
}
Invoke-CheckedCommand 'ctest' $ctest $testArgs

Write-Host ""
Write-Host "All virtio-snd host tests passed."

