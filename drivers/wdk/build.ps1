[CmdletBinding()]
param(
  [string]$Configuration = "Release",
  [string[]]$Platforms = @("Win32", "x64")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-OnWindows {
  if ($env:OS -ne "Windows_NT") {
    throw "This script must be run on Windows (preferably from an EWDK build environment shell)."
  }
}

Assert-OnWindows

$root = Split-Path -Parent $PSScriptRoot
$solutions = Get-ChildItem -Path $root -Recurse -Filter *.sln

if ($solutions.Count -eq 0) {
  Write-Host "No .sln files found under $root"
  Write-Host "Place driver solutions under drivers/wdk/ and re-run."
  exit 0
}

foreach ($sln in $solutions) {
  foreach ($platform in $Platforms) {
    Write-Host "Building $($sln.FullName) ($Configuration|$platform)..."
    & msbuild $sln.FullName /m /p:Configuration=$Configuration /p:Platform=$platform
  }
}

