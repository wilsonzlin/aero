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

.PARAMETER IncludeWdfCoInstaller
  Explicit opt-in to copy Microsoft WDK redistributables (specifically WdfCoInstaller*.dll)
  from the installed WDK into staged driver packages, but only for drivers that declare
  a `wdfCoInstaller` requirement in `drivers/<driver>/ci-package.json`.

.PARAMETER IncludeWdkRedist
  Alternative opt-in mechanism to allow specific WDK redistributables by name
  (example: -IncludeWdkRedist WdfCoInstaller). Default is empty.
#>

#Requires -Version 5.1

[CmdletBinding()]
param(
  [string[]] $OsList = @('7_X86', '7_X64'),
  [string] $InputRoot = 'out/drivers',
  [string] $OutputRoot = 'out/packages',
  [string] $ToolchainJson,
  [switch] $NoStampInfs,
  [switch] $IncludeWdfCoInstaller,
  [string[]] $IncludeWdkRedist = @()
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

function Resolve-ChildPathUnderRoot {
  param(
    [Parameter(Mandatory)]
    [string] $Root,
    [Parameter(Mandatory)]
    [string] $ChildPath
  )

  if ([System.IO.Path]::IsPathRooted($ChildPath)) {
    throw "Path '$ChildPath' must be relative to '$Root'."
  }

  $sep = [System.IO.Path]::DirectorySeparatorChar
  $alt = [System.IO.Path]::AltDirectorySeparatorChar
  $rootResolved = [System.IO.Path]::GetFullPath($Root).TrimEnd($sep, $alt)
  $full = [System.IO.Path]::GetFullPath((Join-Path -Path $rootResolved -ChildPath $ChildPath))

  $prefix = $rootResolved + $sep
  if (-not $full.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path '$ChildPath' resolves outside root '$rootResolved'."
  }

  return $full
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

$allowWdfCoInstaller = $IncludeWdfCoInstaller -or ($IncludeWdkRedist -contains 'WdfCoInstaller')
if ($allowWdfCoInstaller) {
  Write-Host "WDK redistributables enabled: WdfCoInstaller"
} else {
  Write-Host "WDK redistributables disabled (default)."
}

function Read-DriverPackageManifest {
  param(
    [Parameter(Mandatory)]
    [string] $DriverSourceDir
  )

  $manifestPath = Join-Path -Path $DriverSourceDir -ChildPath 'ci-package.json'
  if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    return @{
      ManifestPath = $manifestPath
      AdditionalFiles = @()
      WdfCoInstaller = $null
    }
  }

  $data = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

  $additional = @()
  if ($null -ne $data.additionalFiles) {
    foreach ($entry in @($data.additionalFiles)) {
      if ($null -eq $entry) { continue }
      $s = ([string]$entry).Trim()
      if ($s.Length -gt 0) {
        $additional += $s
      }
    }
  }

  $wdf = $null
  if ($null -ne $data.wdfCoInstaller) {
    $kmdfVersion = [string]$data.wdfCoInstaller.kmdfVersion
    if ([string]::IsNullOrWhiteSpace($kmdfVersion)) {
      throw "Invalid manifest '$manifestPath': wdfCoInstaller.kmdfVersion is required."
    }
    $dllName = [string]$data.wdfCoInstaller.dllName
    if ([string]::IsNullOrWhiteSpace($dllName)) {
      $dllName = $null
    } else {
      $dllName = $dllName.Trim()
      [char[]]$sepChars = @([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
      if ($dllName.IndexOfAny($sepChars) -ge 0) {
        throw "Invalid manifest '$manifestPath': wdfCoInstaller.dllName must be a file name, not a path."
      }
      if ($dllName -notmatch '^WdfCoInstaller\d{5}\.dll$') {
        throw "Invalid manifest '$manifestPath': wdfCoInstaller.dllName must match 'WdfCoInstallerNNNNN.dll' (example: WdfCoInstaller01011.dll)."
      }
    }
    $wdf = @{
      KmdfVersion = $kmdfVersion.Trim()
      DllName = $dllName
    }
  }

  return @{
    ManifestPath = $manifestPath
    AdditionalFiles = $additional
    WdfCoInstaller = $wdf
  }
}

function Get-WdfCoInstallerDllNameFromKmdfVersion {
  param(
    [Parameter(Mandatory)]
    [string] $KmdfVersion
  )

  $parts = $KmdfVersion.Split('.')
  if ($parts.Count -ne 2) {
    throw "Invalid KMDF version '$KmdfVersion' (expected format 'major.minor', e.g. '1.11')."
  }
  $major = [int]$parts[0]
  $minor = [int]$parts[1]
  $digits = "{0:D2}{1:D3}" -f $major, $minor
  return "WdfCoInstaller$digits.dll"
}

function Get-WdkKitRoots {
  $roots = New-Object System.Collections.Generic.List[string]

  foreach ($envVar in @('WindowsSdkDir', 'WindowsSdkDir_10', 'WindowsSdkDir_81')) {
    $value = [Environment]::GetEnvironmentVariable($envVar)
    if ($null -ne $value -and $value.Trim() -ne '') {
      $roots.Add($value.TrimEnd('\'))
    }
  }

  $programFilesX86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
  if ([string]::IsNullOrWhiteSpace($programFilesX86)) {
    $programFilesX86 = [Environment]::GetEnvironmentVariable('ProgramFiles')
  }

  foreach ($kitVersion in @('10', '8.1', '8.0')) {
    $kitRoot = Join-Path -Path $programFilesX86 -ChildPath ("Windows Kits\{0}" -f $kitVersion)
    if (Test-Path -LiteralPath $kitRoot) {
      $roots.Add($kitRoot)
    }
  }

  $winDdkRoot = 'C:\WinDDK'
  if (Test-Path -LiteralPath $winDdkRoot) {
    foreach ($ddk in (Get-ChildItem -LiteralPath $winDdkRoot -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending)) {
      $roots.Add($ddk.FullName)
    }
  }

  return $roots | Select-Object -Unique
}

function Resolve-WdfCoInstallerPath {
  param(
    [Parameter(Mandatory)]
    [string] $DllName,
    [Parameter(Mandatory)]
    [ValidateSet('x86', 'x64')]
    [string] $Arch
  )

  $archDirs = if ($Arch -eq 'x86') { @('x86') } else { @('amd64', 'x64') }
  $archRegex = "\\(" + (($archDirs | ForEach-Object { [Regex]::Escape($_) }) -join '|') + ")\\"

  foreach ($kitRoot in (Get-WdkKitRoots)) {
    foreach ($wdfRoot in @(
      (Join-Path -Path $kitRoot -ChildPath 'Redist\wdf'),
      (Join-Path -Path $kitRoot -ChildPath 'redist\wdf'),
      (Join-Path -Path $kitRoot -ChildPath 'Redist\WDF'),
      (Join-Path -Path $kitRoot -ChildPath 'redist\WDF')
    )) {
      if (-not (Test-Path -LiteralPath $wdfRoot -PathType Container)) {
        continue
      }

      $candidates = Get-ChildItem -LiteralPath $wdfRoot -Recurse -File -Filter $DllName -ErrorAction SilentlyContinue
      $match = $candidates | Where-Object { $_.FullName -match $archRegex } | Select-Object -First 1
      if ($match) {
        return $match.FullName
      }
    }
  }

  return $null
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
  $driverRel = [string]$driverBuildDir.RelativePath
  $driverNameForLog = [string]$driverBuildDir.DisplayName

  Write-Host "==> Driver: $driverNameForLog"

  $driverSourceDir = Join-Path -Path $driversRoot -ChildPath $driverRel
  if (-not (Test-Path -LiteralPath $driverSourceDir)) {
    throw "Driver source directory not found for '$driverNameForLog'. Expected: $driverSourceDir"
  }

  $manifest = Read-DriverPackageManifest -DriverSourceDir $driverSourceDir
  $needsWdfCoInstaller = ($null -ne $manifest.WdfCoInstaller)
  $wdfCoInstallerDllName = $null
  if ($needsWdfCoInstaller) {
    if (-not $allowWdfCoInstaller) {
      throw "Driver '$driverName' declares a WDF coinstaller requirement in '$($manifest.ManifestPath)', but WDK redistributables are disabled. Re-run with -IncludeWdfCoInstaller."
    }
    $wdfCoInstallerDllName = $manifest.WdfCoInstaller.DllName
    if ([string]::IsNullOrWhiteSpace($wdfCoInstallerDllName)) {
      $wdfCoInstallerDllName = Get-WdfCoInstallerDllNameFromKmdfVersion -KmdfVersion $manifest.WdfCoInstaller.KmdfVersion
    }
  }

  $wdfInSource = Get-ChildItem -LiteralPath $driverSourceDir -Recurse -File -Filter 'WdfCoInstaller*.dll' -ErrorAction SilentlyContinue
  if ($wdfInSource) {
    throw "Driver '$driverName' contains WdfCoInstaller*.dll under '$driverSourceDir'. Do not commit Microsoft WDK redistributables into the repo; use -IncludeWdfCoInstaller and '$($manifest.ManifestPath)' instead."
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

    $packageDir = Join-Path -Path $outputRootAbs -ChildPath (Join-Path -Path $driverRel -ChildPath $arch)
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
        throw "Duplicate INF file name '$($inf.Name)' for driver '$driverNameForLog'. Ensure INF names are unique within the driver directory."
      }
      $infNameMap[$inf.Name] = $true

      $destInf = Join-Path -Path $packageDir -ChildPath $inf.Name
      Copy-Item -LiteralPath $inf.FullName -Destination $destInf -Force
      $stagedInfPaths += $destInf
    }

    if ($infNameMap.Count -eq 0) {
      throw "No INF files applicable to $arch were found for driver '$driverNameForLog'."
    }

    foreach ($coName in @('coinstallers', 'coinstaller')) {
      $coDir = Join-Path -Path $driverSourceDir -ChildPath $coName
      if (-not (Test-Path -LiteralPath $coDir)) { continue }

      Copy-Item -LiteralPath $coDir -Destination (Join-Path -Path $packageDir -ChildPath $coName) -Recurse -Force
      Get-ChildItem -LiteralPath $coDir -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path -Path $packageDir -ChildPath $_.Name) -Force
      }
    }

    foreach ($relPath in $manifest.AdditionalFiles) {
      $ext = [IO.Path]::GetExtension($relPath)
      if ($ext -in @('.sys', '.dll', '.exe', '.cat', '.msi', '.cab')) {
        throw "Driver '$driverName' additionalFiles must be non-binary; refusing to include '$relPath'."
      }

      $src = Resolve-ChildPathUnderRoot -Root $driverSourceDir -ChildPath $relPath
      if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
        throw "Driver '$driverName' additional file not found: $src"
      }

      $dest = Resolve-ChildPathUnderRoot -Root $packageDir -ChildPath $relPath
      $destDir = Split-Path -Parent $dest
      if ($destDir -and -not (Test-Path -LiteralPath $destDir)) {
        New-Item -ItemType Directory -Path $destDir -Force | Out-Null
      }
      Copy-Item -LiteralPath $src -Destination $dest -Force
    }

    $existingWdf = @(Get-ChildItem -LiteralPath $packageDir -Recurse -File -Filter 'WdfCoInstaller*.dll' -ErrorAction SilentlyContinue)
    if ($existingWdf.Count -gt 0) {
      if (-not $allowWdfCoInstaller) {
        throw "WDK redistributables are disabled, but staged package '$packageDir' already contains: $($existingWdf.Name -join ', ')"
      }
      if (-not $needsWdfCoInstaller) {
        throw "Staged package '$packageDir' contains WdfCoInstaller*.dll, but driver '$driverName' does not declare wdfCoInstaller in '$($manifest.ManifestPath)'."
      }
      $existingWdf | Remove-Item -Force -ErrorAction Stop
    }

    if ($needsWdfCoInstaller) {
      $wdfSource = Resolve-WdfCoInstallerPath -DllName $wdfCoInstallerDllName -Arch $arch
      if (-not $wdfSource) {
        throw "Unable to locate '$wdfCoInstallerDllName' in installed WDK redistributables for $arch. Ensure the Windows Driver Kit is installed (Windows Kits\\<ver>\\Redist\\wdf)."
      }

      Write-Host "     Including $wdfCoInstallerDllName ($arch) from: $wdfSource"
      Copy-Item -LiteralPath $wdfSource -Destination (Join-Path -Path $packageDir -ChildPath $wdfCoInstallerDllName) -Force
      foreach ($coName in @('coinstallers', 'coinstaller')) {
        $destCoDir = Join-Path -Path $packageDir -ChildPath $coName
        if (-not (Test-Path -LiteralPath $destCoDir -PathType Container)) { continue }
        Copy-Item -LiteralPath $wdfSource -Destination (Join-Path -Path $destCoDir -ChildPath $wdfCoInstallerDllName) -Force
      }
    }

    if ($stampInfs) {
      Write-Host "     Stamping staged INF(s) prior to catalog generation..."
      $stampArgs = @{
        StagingDir = $packageDir
        InfPaths   = $stagedInfPaths
        RepoRoot   = $repoRoot
      }
      if ($toolchainJsonAbs) {
        $stampArgs.ToolchainJson = $toolchainJsonAbs
      }
      & $stampScript @stampArgs | Out-Null
    } else {
      Write-Host "     INF stamping disabled; using existing DriverVer values."
    }

    Invoke-Inf2Cat -Inf2CatPath $inf2catPath -PackageDir $packageDir -OsList $osListForArch

    $cats = Get-ChildItem -LiteralPath $packageDir -Filter '*.cat' -File -Recurse -ErrorAction SilentlyContinue
    if (-not $cats) {
      throw "Inf2Cat did not produce a .cat file for $driverNameForLog ($arch)."
    }
    Write-Host "     Generated catalog(s):"
    foreach ($cat in $cats) {
      Write-Host "       - $($cat.FullName)"
    }
  }
}

