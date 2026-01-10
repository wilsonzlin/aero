#Requires -Version 5.1

<#
.SYNOPSIS
Builds all Aero Windows 7 kernel drivers (x86 + x64) in CI.

.DESCRIPTION
Discovers driver projects under `drivers/<name>/` and builds each driver for the requested
platforms/configuration using MSBuild (command-line only).

Discovery conventions (encoded here for CI determinism):
  - Each driver lives under `drivers/<name>/`.
  - Each driver provides either:
      - a solution file `drivers/<name>/<name>.sln`, OR
      - exactly one project file `drivers/<name>/*.vcxproj`.
  - Build outputs are staged under:
      - `out/drivers/<name>/<arch>/...`

Toolchain:
  - If `-ToolchainJson` is provided, the script will try to use it to locate MSBuild and
    optionally import the Visual Studio developer environment.
  - If not provided, the script expects MSBuild (and the VC/WDK toolchain) to already be
    available in the current environment.
#>

[CmdletBinding()]
param(
  [Parameter()]
  [ValidateNotNullOrEmpty()]
  [string]$Configuration = 'Release',

  [Parameter()]
  [ValidateNotNullOrEmpty()]
  [string[]]$Platforms = @('Win32', 'x64'),

  [Parameter()]
  [string[]]$Drivers,

  [Parameter()]
  [string]$ToolchainJson,

  [Parameter()]
  [switch]$RequireDrivers
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RepoRoot {
  $ciDir = $PSScriptRoot
  if (-not $ciDir) {
    throw "Unable to determine script directory (PSScriptRoot is empty)."
  }
  return (Resolve-Path (Join-Path $ciDir '..')).Path
}

function Get-UniqueStringsInOrder {
  param([string[]]$Values)
  $seen = @{}
  $result = New-Object System.Collections.Generic.List[string]
  foreach ($value in $Values) {
    if ([string]::IsNullOrWhiteSpace($value)) { continue }
    $key = $value.ToLowerInvariant()
    if ($seen.ContainsKey($key)) { continue }
    $seen[$key] = $true
    [void]$result.Add($value)
  }
  return ,$result.ToArray()
}

function Normalize-Platform {
  param([Parameter(Mandatory = $true)][string]$Platform)
  switch ($Platform.Trim().ToLowerInvariant()) {
    'win32' { return 'Win32' }
    'x86' { return 'Win32' }
    'ia32' { return 'Win32' }
    'x64' { return 'x64' }
    'amd64' { return 'x64' }
    default { throw "Unsupported platform '$Platform'. Supported: Win32/x86, x64/amd64." }
  }
}

function Platform-ToArch {
  param([Parameter(Mandatory = $true)][string]$Platform)
  switch ($Platform) {
    'Win32' { return 'x86' }
    'x64' { return 'x64' }
    default { throw "Unsupported normalized platform '$Platform'." }
  }
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Path $Path -Force | Out-Null
}

function Ensure-Directory {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
  }
}

function Read-ToolchainJson {
  param([string]$ToolchainJsonPath)
  if (-not $ToolchainJsonPath) { return $null }
  if (-not (Test-Path -LiteralPath $ToolchainJsonPath -PathType Leaf)) {
    throw "ToolchainJson not found: $ToolchainJsonPath"
  }
  $raw = Get-Content -LiteralPath $ToolchainJsonPath -Raw
  if ([string]::IsNullOrWhiteSpace($raw)) {
    throw "ToolchainJson is empty: $ToolchainJsonPath"
  }
  return ($raw | ConvertFrom-Json)
}

function Get-ToolchainPropertyValue {
  param(
    [Parameter(Mandatory = $true)]$Toolchain,
    [Parameter(Mandatory = $true)][string[]]$Names
  )
  foreach ($name in $Names) {
    if ($null -eq $Toolchain) { continue }
    $prop = $Toolchain.PSObject.Properties[$name]
    if ($null -eq $prop) { continue }
    $value = [string]$prop.Value
    if (-not [string]::IsNullOrWhiteSpace($value)) {
      return $value
    }
  }
  return $null
}

function Import-EnvironmentFromBatchFile {
  param(
    [Parameter(Mandatory = $true)][string]$BatchPath,
    [Parameter()][string[]]$Arguments = @()
  )

  if (-not (Test-Path -LiteralPath $BatchPath -PathType Leaf)) {
    throw "Batch file not found: $BatchPath"
  }

  $quotedArgs = @()
  foreach ($arg in $Arguments) {
    if ($arg -match '[\s"]') {
      $quotedArgs += ('"{0}"' -f $arg.Replace('"', '""'))
    } else {
      $quotedArgs += $arg
    }
  }
  $argString = ($quotedArgs -join ' ')

  # cmd.exe returns all environment variables; we then apply them to the current PowerShell process.
  $cmd = "call `"$BatchPath`" $argString >nul 2>nul && set"
  $lines = & cmd.exe /d /s /c $cmd
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to import environment via: $BatchPath $argString"
  }

  foreach ($line in $lines) {
    if ($line -notmatch '^(.*?)=(.*)$') { continue }
    $name = $Matches[1]
    $value = $Matches[2]
    if ([string]::IsNullOrEmpty($name)) { continue }
    if ($name.StartsWith('=')) { continue } # cmd.exe uses these internally (e.g. '=C:')
    Set-Item -Path "Env:$name" -Value $value
  }
}

function Initialize-ToolchainEnvironment {
  param(
    $Toolchain,
    [string[]]$NormalizedPlatforms
  )

  if ($null -eq $Toolchain) { return }

  $envMap = $null
  $prop = $Toolchain.PSObject.Properties['Env']
  if ($null -ne $prop) { $envMap = $prop.Value }
  if ($null -eq $envMap) {
    $prop = $Toolchain.PSObject.Properties['env']
    if ($null -ne $prop) { $envMap = $prop.Value }
  }
  if ($null -eq $envMap) {
    $prop = $Toolchain.PSObject.Properties['Environment']
    if ($null -ne $prop) { $envMap = $prop.Value }
  }
  if ($null -eq $envMap) {
    $prop = $Toolchain.PSObject.Properties['environment']
    if ($null -ne $prop) { $envMap = $prop.Value }
  }

  if ($null -ne $envMap) {
    foreach ($prop in $envMap.PSObject.Properties) {
      Set-Item -Path ("Env:{0}" -f $prop.Name) -Value ([string]$prop.Value)
    }
  }

  $vsDevCmd = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VsDevCmd', 'vsDevCmd', 'vsdevcmd')
  $vcVarsAll = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VcVarsAll', 'vcVarsAll', 'vcvarsall')

  # If we have a dev-environment batch file, use it to populate PATH/INCLUDE/LIB/etc.
  # For simplicity we import a single environment that is sufficient for building both Win32 and x64.
  if ($vsDevCmd) {
    $arch = if ($NormalizedPlatforms -contains 'x64') { 'x64' } else { 'x86' }
    Write-Host "Importing Visual Studio environment via VsDevCmd: $vsDevCmd (-arch=$arch -host_arch=$arch)"
    Import-EnvironmentFromBatchFile -BatchPath $vsDevCmd -Arguments @("-arch=$arch", "-host_arch=$arch")
  } elseif ($vcVarsAll) {
    $archArg = if ($NormalizedPlatforms -contains 'x64') { 'amd64' } else { 'x86' }
    Write-Host "Importing Visual Studio environment via vcvarsall: $vcVarsAll ($archArg)"
    Import-EnvironmentFromBatchFile -BatchPath $vcVarsAll -Arguments @($archArg)
  }
}

function Resolve-MSBuildPath {
  param($Toolchain)

  $fromJson = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @(
    'MSBuild', 'MsBuild', 'msbuild', 'MSBuildPath', 'MsBuildPath', 'msbuildPath'
  )
  if ($fromJson) {
    $expanded = [Environment]::ExpandEnvironmentVariables($fromJson)
    if (Test-Path -LiteralPath $expanded -PathType Leaf) {
      return (Resolve-Path -LiteralPath $expanded).Path
    }
    throw "ToolchainJson MSBuild path does not exist: $fromJson"
  }

  $cmd = Get-Command -Name 'msbuild.exe' -ErrorAction SilentlyContinue
  if ($cmd) {
    return $cmd.Source
  }

  $cmd = Get-Command -Name 'msbuild' -ErrorAction SilentlyContinue
  if ($cmd) {
    return $cmd.Source
  }

  $vswhere = $null
  if (Test-Path -LiteralPath "${env:ProgramFiles(x86)}\\Microsoft Visual Studio\\Installer\\vswhere.exe" -PathType Leaf) {
    $vswhere = "${env:ProgramFiles(x86)}\\Microsoft Visual Studio\\Installer\\vswhere.exe"
  }

  if ($vswhere) {
    $installPath = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
    if ($LASTEXITCODE -ne 0) {
      throw "vswhere failed with exit code $LASTEXITCODE"
    }
    $installPath = [string]$installPath
    $installPath = $installPath.Trim()
    if (-not [string]::IsNullOrWhiteSpace($installPath)) {
      $candidates = @(
        (Join-Path $installPath 'MSBuild\\Current\\Bin\\amd64\\MSBuild.exe'),
        (Join-Path $installPath 'MSBuild\\Current\\Bin\\MSBuild.exe'),
        (Join-Path $installPath 'MSBuild\\15.0\\Bin\\amd64\\MSBuild.exe'),
        (Join-Path $installPath 'MSBuild\\15.0\\Bin\\MSBuild.exe')
      )
      foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
          return (Resolve-Path -LiteralPath $candidate).Path
        }
      }
    }
  }

  throw "MSBuild not found. Install Visual Studio Build Tools/WDK or provide -ToolchainJson with an MSBuild path."
}

function Discover-DriverBuildTargets {
  param(
    [Parameter(Mandatory = $true)][string]$DriversRoot,
    [string[]]$AllowList
  )

  if (-not (Test-Path -LiteralPath $DriversRoot -PathType Container)) {
    return @()
  }

  $allDirs = @(Get-ChildItem -LiteralPath $DriversRoot -Directory)
  $selectedDirs = @()

  if ($AllowList -and $AllowList.Count -gt 0) {
    foreach ($name in $AllowList) {
      $dir = $allDirs | Where-Object { $_.Name -ieq $name } | Select-Object -First 1
      if (-not $dir) {
        throw "Requested driver '$name' not found under: $DriversRoot"
      }
      $selectedDirs += $dir
    }
  } else {
    $selectedDirs = $allDirs | Sort-Object -Property Name
  }

  $targets = New-Object System.Collections.Generic.List[object]

  foreach ($dir in $selectedDirs) {
    $name = $dir.Name
    $sln = Join-Path $dir.FullName ("{0}.sln" -f $name)
    $buildPath = $null
    $kind = $null

    if (Test-Path -LiteralPath $sln -PathType Leaf) {
      $buildPath = $sln
      $kind = 'sln'
    } else {
      $vcxprojs = @(Get-ChildItem -LiteralPath $dir.FullName -File -Filter '*.vcxproj')
      if ($vcxprojs.Count -eq 1) {
        $buildPath = $vcxprojs[0].FullName
        $kind = 'vcxproj'
      } elseif ($vcxprojs.Count -eq 0) {
        if ($AllowList) {
          throw "Driver '$name' has no '$name.sln' and no '*.vcxproj' under: $($dir.FullName)"
        }
        Write-Host "Skipping drivers/$name: no '$name.sln' or '*.vcxproj' found."
        continue
      } else {
        throw "Driver '$name' has multiple '*.vcxproj' files but no '$name.sln' under: $($dir.FullName)"
      }
    }

    [void]$targets.Add([pscustomobject]@{
      Name = $name
      Kind = $kind
      BuildPath = (Resolve-Path -LiteralPath $buildPath).Path
      Directory = (Resolve-Path -LiteralPath $dir.FullName).Path
    })
  }

  return ,$targets.ToArray()
}

function Format-CommandLine {
  param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string[]]$Arguments
  )
  $parts = @()
  $parts += ('"{0}"' -f $Exe.Replace('"', '""'))
  foreach ($arg in $Arguments) {
    if ($arg -match '[\s"]') {
      $parts += ('"{0}"' -f $arg.Replace('"', '""'))
    } else {
      $parts += $arg
    }
  }
  return ($parts -join ' ')
}

function Invoke-MSBuild {
  param(
    [Parameter(Mandatory = $true)][string]$MSBuildPath,
    [Parameter(Mandatory = $true)][string]$ProjectOrSolutionPath,
    [Parameter(Mandatory = $true)][string]$Configuration,
    [Parameter(Mandatory = $true)][string]$Platform,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [Parameter(Mandatory = $true)][string]$ObjDir,
    [Parameter(Mandatory = $true)][string]$LogFile,
    [Parameter(Mandatory = $true)][string]$BinLogFile
  )

  $sep = [IO.Path]::DirectorySeparatorChar
  $outDirNormalized = $OutDir.TrimEnd($sep) + $sep
  $objDirNormalized = $ObjDir.TrimEnd($sep) + $sep

  $args = @(
    $ProjectOrSolutionPath,
    '/m',
    '/nologo',
    '/t:Build',
    "/p:Configuration=$Configuration",
    "/p:Platform=$Platform",
    "/p:OutDir=$outDirNormalized",
    "/p:BaseIntermediateOutputPath=$objDirNormalized",
    '/p:GenerateFullPaths=true',
    "/bl:$BinLogFile",
    '/fl',
    "/flp:logfile=$LogFile;verbosity=normal",
    '/clp:Summary;NoItemAndPropertyList'
  )

  Write-Host (Format-CommandLine -Exe $MSBuildPath -Arguments $args)

  & $MSBuildPath @args
  $exitCode = $LASTEXITCODE

  return $exitCode
}

$repoRoot = Get-RepoRoot
$driversRoot = Join-Path $repoRoot 'drivers'
$outRoot = Join-Path $repoRoot 'out'
$outDriversRoot = Join-Path $outRoot 'drivers'
$logRoot = Join-Path $outRoot 'logs\\drivers'
$objRoot = Join-Path $outRoot 'obj\\drivers'

$normalizedPlatforms = @()
foreach ($p in $Platforms) { $normalizedPlatforms += (Normalize-Platform -Platform $p) }
$normalizedPlatforms = Get-UniqueStringsInOrder -Values $normalizedPlatforms

Write-Host "Configuration: $Configuration"
Write-Host ("Platforms: {0}" -f ($normalizedPlatforms -join ', '))
if ($Drivers) { Write-Host ("Drivers allowlist: {0}" -f ($Drivers -join ', ')) }

$toolchain = Read-ToolchainJson -ToolchainJsonPath $ToolchainJson
Initialize-ToolchainEnvironment -Toolchain $toolchain -NormalizedPlatforms $normalizedPlatforms
$msbuild = Resolve-MSBuildPath -Toolchain $toolchain
Write-Host "Using MSBuild: $msbuild"

$targets = Discover-DriverBuildTargets -DriversRoot $driversRoot -AllowList $Drivers

if (-not $targets -or $targets.Count -eq 0) {
  $msg = "no driver projects found"
  if (-not (Test-Path -LiteralPath $driversRoot -PathType Container)) {
    $msg = "$msg (drivers/ directory does not exist)"
  }
  if ($RequireDrivers) {
    throw $msg
  }
  Write-Host $msg
  exit 0
}

Ensure-Directory -Path $outDriversRoot
Ensure-Directory -Path $logRoot
Ensure-Directory -Path $objRoot

$results = New-Object System.Collections.Generic.List[object]
$failed = $false

foreach ($target in $targets) {
  foreach ($platform in $normalizedPlatforms) {
    $arch = Platform-ToArch -Platform $platform

    $driverOutDir = Join-Path $outDriversRoot (Join-Path $target.Name $arch)
    $driverObjDir = Join-Path $objRoot (Join-Path $target.Name $arch)

    Ensure-EmptyDirectory -Path $driverOutDir
    Ensure-EmptyDirectory -Path $driverObjDir

    $logFile = Join-Path $logRoot ("{0}-{1}.msbuild.log" -f $target.Name, $arch)
    $binLogFile = Join-Path $logRoot ("{0}-{1}.msbuild.binlog" -f $target.Name, $arch)

    Write-Host ""
    Write-Host ("==> Building driver '{0}' ({1}) [{2}|{3}]" -f $target.Name, $target.Kind, $Configuration, $platform)
    Write-Host ("    Project: {0}" -f $target.BuildPath)
    Write-Host ("    Output:  {0}" -f $driverOutDir)

    $exitCode = Invoke-MSBuild `
      -MSBuildPath $msbuild `
      -ProjectOrSolutionPath $target.BuildPath `
      -Configuration $Configuration `
      -Platform $platform `
      -OutDir $driverOutDir `
      -ObjDir $driverObjDir `
      -LogFile $logFile `
      -BinLogFile $binLogFile

    $succeeded = ($exitCode -eq 0)
    if (-not $succeeded) {
      $failed = $true
      Write-Host ("!! Build FAILED for driver '{0}' Platform={1} (exit code {2})" -f $target.Name, $platform, $exitCode)
      Write-Host ("   MSBuild log:   {0}" -f $logFile)
      Write-Host ("   MSBuild binlog:{0}" -f $binLogFile)
    }

    [void]$results.Add([pscustomobject]@{
      Driver = $target.Name
      Platform = $platform
      Arch = $arch
      OutputPath = $driverOutDir
      Project = $target.BuildPath
      Log = $logFile
      BinLog = $binLogFile
      Succeeded = $succeeded
    })
  }
}

Write-Host ""
Write-Host "Driver build summary:"

foreach ($driverName in ($results | Select-Object -ExpandProperty Driver | Sort-Object -Unique)) {
  Write-Host ("- {0}" -f $driverName)
  foreach ($r in ($results | Where-Object { $_.Driver -eq $driverName } | Sort-Object -Property Arch)) {
    $status = if ($r.Succeeded) { 'OK' } else { 'FAIL' }
    Write-Host ("    {0}: {1}" -f $r.Arch, $r.OutputPath)
    Write-Host ("      status: {0}" -f $status)
    Write-Host ("      log:    {0}" -f $r.Log)
  }
}

if ($failed) {
  exit 1
}

exit 0
