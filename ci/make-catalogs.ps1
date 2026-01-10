<#
.SYNOPSIS
  Generates Windows 7 catalog (.cat) files for each staged driver package.

.DESCRIPTION
  This script combines driver packaging assets (INF + optional coinstallers) from the
  repository `drivers/<driver>` directories with built binaries from `out/drivers/<driver>`
  and runs Inf2Cat to produce catalog files in a stable staging layout under `out/packages`.

  `<driver>` is a path relative to the `drivers/` directory. This supports both layouts:
    - `drivers/<name>/...`
    - `drivers/<group>/<name>/...` (e.g. `drivers/windows7/virtio-input/...`)

  If enabled (default), it stamps DriverVer in the staged INF(s) using `ci/stamp-infs.ps1`
  before running Inf2Cat. Catalogs hash INF contents, so stamping must happen first.

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

.PARAMETER NoStampInfs
  Disables stamping DriverVer in staged INFs before catalog generation. You can also set
  AERO_STAMP_INFS=0/false/no/off to disable stamping in CI without changing arguments.
#>

#Requires -Version 5.1

[CmdletBinding()]
param(
  [string[]] $OsList = @('7_X86', '7_X64'),
  [string] $InputRoot = 'out/drivers',
  [string] $OutputRoot = 'out/packages',
  [string] $ToolchainJson,
  [switch] $NoStampInfs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module -Force (Join-Path -Path $PSScriptRoot -ChildPath 'lib/Catalog.psm1')

$repoRoot = Resolve-Path (Join-Path -Path $PSScriptRoot -ChildPath '..') | Select-Object -ExpandProperty Path

function Resolve-RepoPath {
  param(
    [Parameter(Mandatory)]
    [string] $Path
  )

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  return [System.IO.Path]::GetFullPath((Join-Path -Path $repoRoot -ChildPath $Path))
}

function Get-RelativePathFromRoot {
  param(
    [Parameter(Mandatory)]
    [string] $Root,
    [Parameter(Mandatory)]
    [string] $Path
  )

  $sep = [System.IO.Path]::DirectorySeparatorChar
  $alt = [System.IO.Path]::AltDirectorySeparatorChar

  $rootResolved = (Resolve-Path -LiteralPath $Root | Select-Object -ExpandProperty Path).TrimEnd($sep, $alt)
  $pathResolved = (Resolve-Path -LiteralPath $Path | Select-Object -ExpandProperty Path).TrimEnd($sep, $alt)

  $prefix = $rootResolved + $sep
  if (-not $pathResolved.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path '$pathResolved' is not under root '$rootResolved'."
  }

  return $pathResolved.Substring($prefix.Length)
}

function Find-DriverBuildDirs {
  param(
    [Parameter(Mandatory)]
    [string] $InputRoot
  )

  if (-not (Test-Path -LiteralPath $InputRoot)) {
    throw "InputRoot not found: $InputRoot"
  }

  # A driver build directory is expected to contain per-arch subdirectories (x86/x64, etc).
  # We identify driver roots by scanning for directories which have an immediate child directory
  # matching a known architecture directory name.
  $archDirNames = @(
    'x86', 'X86', 'Win32', 'win32', 'i386', 'I386',
    'x64', 'X64', 'amd64', 'AMD64', 'x86_64', 'X86_64',
    '7_X86', '7_X64', 'Server2008R2_X64'
  )

  $seen = @{}
  $results = New-Object System.Collections.Generic.List[object]

  $dirs = @(Get-ChildItem -LiteralPath $InputRoot -Directory -Recurse -ErrorAction SilentlyContinue)
  foreach ($dir in $dirs) {
    $children = @(Get-ChildItem -LiteralPath $dir.FullName -Directory -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
    if (-not $children -or $children.Count -eq 0) { continue }

    $hasArchChild = $false
    foreach ($child in $children) {
      if ($archDirNames -contains $child) {
        $hasArchChild = $true
        break
      }
    }
    if (-not $hasArchChild) { continue }

    $rel = Get-RelativePathFromRoot -Root $InputRoot -Path $dir.FullName
    $key = $rel.ToLowerInvariant()
    if ($seen.ContainsKey($key)) { continue }
    $seen[$key] = $true

    [void]$results.Add([pscustomobject]@{
      RelativePath = $rel
      FullName = $dir.FullName
      DisplayName = $rel.Replace([System.IO.Path]::DirectorySeparatorChar, '/').Replace([System.IO.Path]::AltDirectorySeparatorChar, '/')
    })
  }

  return ($results | Sort-Object -Property RelativePath)
}

function Get-TruthyEnvFlag {
  param([Parameter(Mandatory = $true)][string] $Name)

  $raw = [Environment]::GetEnvironmentVariable($Name)
  if (-not $raw) {
    return $null
  }

  switch ($raw.Trim().ToLowerInvariant()) {
    '0' { return $false }
    'false' { return $false }
    'no' { return $false }
    'off' { return $false }
    default { return $true }
  }
}

$stampInfs = $true
if ($NoStampInfs) {
  $stampInfs = $false
} else {
  $envStamp = Get-TruthyEnvFlag -Name 'AERO_STAMP_INFS'
  if ($envStamp -eq $false) {
    $stampInfs = $false
  }
}

$envInf2CatOs = [Environment]::GetEnvironmentVariable('AERO_INF2CAT_OS')
if (-not [string]::IsNullOrWhiteSpace($envInf2CatOs)) {
  if ($PSBoundParameters.ContainsKey('OsList')) {
    Write-Host "AERO_INF2CAT_OS is set; overriding -OsList with: $envInf2CatOs"
  } else {
    Write-Host "Using AERO_INF2CAT_OS: $envInf2CatOs"
  }
  $OsList = @($envInf2CatOs)
}

$inputRootAbs = Resolve-RepoPath -Path $InputRoot
$outputRootAbs = Resolve-RepoPath -Path $OutputRoot
$toolchainJsonAbs = if ($ToolchainJson) { Resolve-RepoPath -Path $ToolchainJson } else { $null }

if (-not (Test-Path -LiteralPath $inputRootAbs)) {
  throw "InputRoot not found: $inputRootAbs"
}

New-Item -ItemType Directory -Path $outputRootAbs -Force | Out-Null

$osByArch = Split-OsListByArchitecture -OsList $OsList

$inf2catPath = Resolve-Inf2CatPath -ToolchainJson $toolchainJsonAbs
Write-Host "Using Inf2Cat: $inf2catPath"

$driversRoot = Join-Path -Path $repoRoot -ChildPath 'drivers'

$driverBuildDirs = @(Find-DriverBuildDirs -InputRoot $inputRootAbs)
if (-not $driverBuildDirs) {
  throw "No driver build directories found under $inputRootAbs"
}

$stampScript = Join-Path -Path $PSScriptRoot -ChildPath 'stamp-infs.ps1'

foreach ($driverBuildDir in $driverBuildDirs) {
  $driverName = $driverBuildDir.DisplayName
  Write-Host "==> Driver: $driverName"

  $driverSourceDir = Join-Path -Path $driversRoot -ChildPath $driverBuildDir.RelativePath
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

    $packageDir = Join-Path -Path $outputRootAbs -ChildPath (Join-Path -Path $driverBuildDir.RelativePath -ChildPath $arch)
    Write-Host "  -> Staging package: $packageDir"

    if (Test-Path -LiteralPath $packageDir) {
      Remove-Item -LiteralPath $packageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $packageDir -Force | Out-Null

    $buildOutDir = Resolve-DriverBuildOutputDir -DriverBuildDir $driverBuildDir.FullName -Arch $arch -OsListForArch $osListForArch
    Write-Host "     Using build outputs: $buildOutDir"

    Copy-Item -Path (Join-Path -Path $buildOutDir -ChildPath '*') -Destination $packageDir -Recurse -Force -ErrorAction Stop

    $infNameMap = @{}
    $stagedInfPaths = @()
    foreach ($inf in $infFiles) {
      $support = Get-InfArchitectureSupport -InfPath $inf.FullName
      if ($support -ne 'both' -and $support -ne $arch) { continue }

      if ($infNameMap.ContainsKey($inf.Name)) {
        throw "Duplicate INF file name '$($inf.Name)' for driver '$driverName'. Ensure INF names are unique within the driver directory."
      }
      $infNameMap[$inf.Name] = $true

      $destInf = Join-Path -Path $packageDir -ChildPath $inf.Name
      Copy-Item -LiteralPath $inf.FullName -Destination $destInf -Force
      $stagedInfPaths += $destInf
    }

    if ($infNameMap.Count -eq 0) {
      throw "No INF files applicable to $arch were found for driver '$driverName'."
    }

    foreach ($coName in @('coinstallers', 'coinstaller')) {
      $coDir = Join-Path -Path $driverSourceDir -ChildPath $coName
      if (-not (Test-Path -LiteralPath $coDir)) { continue }

      Copy-Item -LiteralPath $coDir -Destination (Join-Path -Path $packageDir -ChildPath $coName) -Recurse -Force
      Get-ChildItem -LiteralPath $coDir -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path -Path $packageDir -ChildPath $_.Name) -Force
      }
    }

    if ($stampInfs) {
      Write-Host "     Stamping staged INF(s) prior to catalog generation..."
      $stampArgs = @{
        StagingDir = $packageDir
        InfPaths   = $stagedInfPaths
        RepoRoot   = $repoRoot
      }
      if ($ToolchainJson) {
        $stampArgs.ToolchainJson = $ToolchainJson
      }
      & $stampScript @stampArgs | Out-Null
    } else {
      Write-Host "     INF stamping disabled; using existing DriverVer values."
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

