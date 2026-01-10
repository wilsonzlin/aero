<#
.SYNOPSIS
  Generates Windows 7 catalog (.cat) files for each staged driver package.

.DESCRIPTION
  This script combines driver packaging assets (INF + optional coinstallers) from the
  repository `drivers/<name>` directories with built binaries from `out/drivers/<name>`
  and runs Inf2Cat to produce catalog files in a stable staging layout under `out/packages`.

  The output staging folders are intended to be consumed by later signing/packaging steps.

.PARAMETER OsList
  List of OS identifiers to pass to Inf2Cat. Defaults to @('7_X86','7_X64').
  You may also include Server2008R2_X64 (it will be grouped into the x64 package).

.PARAMETER InputRoot
  Root directory containing staged build outputs (per driver). Defaults to out/drivers.

.PARAMETER OutputRoot
  Root directory to write staged packages (per driver + arch). Defaults to out/packages.

.PARAMETER ToolchainJson
  Optional JSON file describing toolchain paths. If provided, the script will try to
  discover Inf2Cat.exe from it.
#>

[CmdletBinding()]
param(
  [string[]] $OsList = @('7_X86', '7_X64'),
  [string] $InputRoot = 'out/drivers',
  [string] $OutputRoot = 'out/packages',
  [string] $ToolchainJson
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module -Force (Join-Path -Path $PSScriptRoot -ChildPath 'lib/Catalog.psm1')

function Resolve-AbsolutePath {
  param(
    [Parameter(Mandatory)]
    [string] $Path
  )

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return $Path
  }

  return Join-Path -Path (Get-Location) -ChildPath $Path
}

$inputRootAbs = Resolve-AbsolutePath -Path $InputRoot
$outputRootAbs = Resolve-AbsolutePath -Path $OutputRoot

if (-not (Test-Path -LiteralPath $inputRootAbs)) {
  throw "InputRoot not found: $inputRootAbs"
}

New-Item -ItemType Directory -Path $outputRootAbs -Force | Out-Null

$osByArch = Split-OsListByArchitecture -OsList $OsList

$inf2catPath = Resolve-Inf2CatPath -ToolchainJson $ToolchainJson
Write-Host "Using Inf2Cat: $inf2catPath"

$repoRoot = Resolve-Path (Join-Path -Path $PSScriptRoot -ChildPath '..') | Select-Object -ExpandProperty Path
$driversRoot = Join-Path -Path $repoRoot -ChildPath 'drivers'

$driverBuildDirs = Get-ChildItem -LiteralPath $inputRootAbs -Directory | Sort-Object -Property Name
if (-not $driverBuildDirs) {
  throw "No driver build directories found under $inputRootAbs"
}

foreach ($driverBuildDir in $driverBuildDirs) {
  $driverName = $driverBuildDir.Name
  Write-Host "==> Driver: $driverName"

  $driverSourceDir = Join-Path -Path $driversRoot -ChildPath $driverName
  if (-not (Test-Path -LiteralPath $driverSourceDir)) {
    throw "Driver source directory not found for '$driverName'. Expected: $driverSourceDir"
  }

  $infFiles = Get-ChildItem -LiteralPath $driverSourceDir -Recurse -File -Filter '*.inf' |
    Where-Object { $_.FullName -notmatch '[\\\\/](obj|out|build|target)[\\\\/]' } |
    Sort-Object -Property FullName

  if (-not $infFiles) {
    throw "No INF files found under $driverSourceDir"
  }

  foreach ($arch in @('x86', 'x64')) {
    $osListForArch = $osByArch[$arch]
    if (-not $osListForArch -or $osListForArch.Count -eq 0) { continue }

    $packageDir = Join-Path -Path $outputRootAbs -ChildPath (Join-Path -Path $driverName -ChildPath $arch)
    Write-Host "  -> Staging package: $packageDir"

    if (Test-Path -LiteralPath $packageDir) {
      Remove-Item -LiteralPath $packageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $packageDir -Force | Out-Null

    $buildOutDir = Resolve-DriverBuildOutputDir -DriverBuildDir $driverBuildDir.FullName -Arch $arch -OsListForArch $osListForArch
    Write-Host "     Using build outputs: $buildOutDir"

    Copy-Item -Path (Join-Path -Path $buildOutDir -ChildPath '*') -Destination $packageDir -Recurse -Force -ErrorAction Stop

    # Copy INF(s) into the package root to keep the staging layout stable and to ensure
    # Inf2Cat emits catalog files directly into the package directory.
    $infNameMap = @{}
    foreach ($inf in $infFiles) {
      $support = Get-InfArchitectureSupport -InfPath $inf.FullName
      if ($support -ne 'both' -and $support -ne $arch) { continue }

      if ($infNameMap.ContainsKey($inf.Name)) {
        throw "Duplicate INF file name '$($inf.Name)' for driver '$driverName'. Ensure INF names are unique within the driver directory."
      }
      $infNameMap[$inf.Name] = $true

      Copy-Item -LiteralPath $inf.FullName -Destination (Join-Path -Path $packageDir -ChildPath $inf.Name) -Force
    }

    if ($infNameMap.Count -eq 0) {
      throw "No INF files applicable to $arch were found for driver '$driverName'."
    }

    # If the driver ships coinstallers, include them in the package. We copy the
    # directory (for INFs that reference a subdir) and also flatten files into the root
    # (for INFs that reference by basename).
    foreach ($coName in @('coinstallers', 'coinstaller')) {
      $coDir = Join-Path -Path $driverSourceDir -ChildPath $coName
      if (-not (Test-Path -LiteralPath $coDir)) { continue }

      Copy-Item -LiteralPath $coDir -Destination (Join-Path -Path $packageDir -ChildPath $coName) -Recurse -Force
      Get-ChildItem -LiteralPath $coDir -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path -Path $packageDir -ChildPath $_.Name) -Force
      }
    }

    Invoke-Inf2Cat -Inf2CatPath $inf2catPath -PackageDir $packageDir -OsList $osListForArch

    $cats = Get-ChildItem -LiteralPath $packageDir -Filter '*.cat' -File -Recurse -ErrorAction SilentlyContinue
    if (-not $cats) {
      throw "Inf2Cat did not produce a .cat file for $driverName ($arch)."
    }
    Write-Host "     Generated catalog(s):"
    foreach ($cat in $cats) {
      Write-Host "       - $($cat.FullName)"
    }
  }
}

