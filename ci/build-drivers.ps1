#Requires -Version 5.1

<#
.SYNOPSIS
Builds all Aero Windows 7 kernel drivers (x86 + x64) in CI.

.DESCRIPTION
Discovers CI-buildable driver projects under `drivers/` and builds each driver for the requested
platforms/configuration using MSBuild (command-line only).

Discovery conventions (encoded here for CI determinism):
  - Drivers may live at any depth under `drivers/` (example: `drivers/windows7/virtio/net`).
  - Each driver directory provides either:
      - a solution file `<dir>/<dirName>.sln`, OR
      - exactly one project file `<dir>/*.vcxproj`.
  - Only CI-buildable drivers are selected:
      - Require at least one `.inf` somewhere under the driver directory tree (excluding
        common build-output directories: `obj/`, `out/`, `build/`, `target/`).
      - Skip WDK 7.1 "NMake wrapper" projects (Keyword=MakeFileProj / ConfigurationType=Makefile).
  - Build outputs are staged under:
      - `out/drivers/<driver-relative-path>/<arch>/...`

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
    [Parameter(Mandatory = $true)][string]$Platform
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
  # We import an environment per *target* platform so that building both Win32 and x64 works reliably.
  if ($vsDevCmd) {
    $hostVsArch = 'x64'
    $procArch = [string]$env:PROCESSOR_ARCHITECTURE
    if ($procArch -and $procArch.Trim().ToLowerInvariant() -eq 'x86') {
      $hostVsArch = 'x86'
    }

    $targetVsArch = if ($Platform -eq 'Win32') { 'x86' } else { 'x64' }

    Write-Host "Importing Visual Studio environment via VsDevCmd: $vsDevCmd (-arch=$targetVsArch -host_arch=$hostVsArch)"
    Import-EnvironmentFromBatchFile -BatchPath $vsDevCmd -Arguments @("-arch=$targetVsArch", "-host_arch=$hostVsArch")
  } elseif ($vcVarsAll) {
    $hostVcArch = 'amd64'
    $procArch = [string]$env:PROCESSOR_ARCHITECTURE
    if ($procArch -and $procArch.Trim().ToLowerInvariant() -eq 'x86') {
      $hostVcArch = 'x86'
    }

    if ($Platform -eq 'Win32') {
      $archArg = if ($hostVcArch -eq 'amd64') { 'amd64_x86' } else { 'x86' }
    } else {
      $archArg = if ($hostVcArch -eq 'amd64') { 'amd64' } else { 'x86_amd64' }
    }

    Write-Host "Importing Visual Studio environment via vcvarsall: $vcVarsAll ($archArg)"
    Import-EnvironmentFromBatchFile -BatchPath $vcVarsAll -Arguments @($archArg)
  }
}

function Resolve-MSBuildPath {
  param($Toolchain)

  $fromJson = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @(
    'MSBuildExe', 'MsBuildExe', 'msbuildExe',
    'MSBuild', 'MsBuild', 'msbuild',
    'MSBuildPath', 'MsBuildPath', 'msbuildPath'
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

function Get-SafeFileName {
  param([Parameter(Mandatory = $true)][string]$Value)
  $result = $Value
  foreach ($c in [IO.Path]::GetInvalidFileNameChars()) {
    $result = $result.Replace([string]$c, '_')
  }
  $result = $result.Replace('/', '_').Replace('\', '_')
  return $result
}

function Find-FirstFileInTree {
  param(
    [Parameter(Mandatory = $true)][string]$Root,
    [Parameter(Mandatory = $true)][string]$Filter,
    [string[]]$ExcludeDirectoryNames = @('obj', 'out', 'build', 'target')
  )

  if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
    return $null
  }

  $exclude = @{}
  foreach ($name in $ExcludeDirectoryNames) {
    if ([string]::IsNullOrWhiteSpace($name)) { continue }
    $exclude[$name.Trim().ToLowerInvariant()] = $true
  }

  $stack = New-Object System.Collections.Generic.Stack[string]
  $stack.Push((Resolve-Path -LiteralPath $Root).Path)

  while ($stack.Count -gt 0) {
    $current = $stack.Pop()

    $files = @(Get-ChildItem -LiteralPath $current -File -Filter $Filter -ErrorAction SilentlyContinue)
    if ($files.Count -gt 0) {
      return $files[0]
    }

    $dirs = @(Get-ChildItem -LiteralPath $current -Directory -ErrorAction SilentlyContinue)
    foreach ($dir in $dirs) {
      $dirName = $dir.Name.ToLowerInvariant()
      if ($exclude.ContainsKey($dirName)) { continue }
      $stack.Push($dir.FullName)
    }
  }

  return $null
}

function Test-IsMakefileVcxproj {
  param([Parameter(Mandatory = $true)][string]$VcxprojPath)

  if (-not (Test-Path -LiteralPath $VcxprojPath -PathType Leaf)) {
    return $false
  }

  $content = Get-Content -LiteralPath $VcxprojPath -Raw -ErrorAction SilentlyContinue
  if ([string]::IsNullOrWhiteSpace($content)) {
    return $false
  }

  return ($content -match '<Keyword>\s*MakeFileProj\s*</Keyword>' -or $content -match '<ConfigurationType>\s*Makefile\s*</ConfigurationType>')
}

function Test-IsMakefileSolution {
  param([Parameter(Mandatory = $true)][string]$SolutionPath)

  if (-not (Test-Path -LiteralPath $SolutionPath -PathType Leaf)) {
    return $false
  }

  $slnDir = Split-Path -Parent $SolutionPath
  $lines = Get-Content -LiteralPath $SolutionPath -ErrorAction SilentlyContinue
  if (-not $lines) {
    return $false
  }

  $projectPaths = New-Object System.Collections.Generic.List[string]
  foreach ($line in $lines) {
    # Example:
    # Project("{GUID}") = "name", "path\\to\\proj.vcxproj", "{GUID}"
    if ($line -match '^\s*Project\(\".*?\"\)\s*=\s*\".*?\"\s*,\s*\"(.*?)\"\s*,') {
      $rel = $Matches[1]
      if ($rel -and $rel.ToLowerInvariant().EndsWith('.vcxproj')) {
        $full = Join-Path -Path $slnDir -ChildPath $rel
        [void]$projectPaths.Add($full)
      }
    }
  }

  if ($projectPaths.Count -eq 0) {
    return $false
  }

  foreach ($proj in $projectPaths) {
    if (Test-IsMakefileVcxproj -VcxprojPath $proj) {
      return $true
    }
  }

  return $false
}

function Test-HasInfInTree {
  param([Parameter(Mandatory = $true)][string]$DirectoryPath)

  $inf = Find-FirstFileInTree -Root $DirectoryPath -Filter '*.inf'
  return ($null -ne $inf)
}

function Try-GetDriverBuildTargetFromDirectory {
  param(
    [Parameter(Mandatory = $true)][System.IO.DirectoryInfo]$Directory,
    [Parameter(Mandatory = $true)][string]$DriversRootResolved
  )

  $name = $Directory.Name
  $sln = Join-Path $Directory.FullName ("{0}.sln" -f $name)
  $buildPath = $null
  $kind = $null

  if (Test-Path -LiteralPath $sln -PathType Leaf) {
    # Ignore solutions that only contain legacy Makefile wrapper projects.
    $projectRelPaths = @()
    foreach ($line in (Get-Content -LiteralPath $sln -ErrorAction SilentlyContinue)) {
      if ($line -match '^Project\(\"{[^}]+}\"\)\s*=\s*\"[^\"]+\",\s*\"([^\"]+\.vcxproj)\"') {
        $projectRelPaths += $Matches[1]
      }
    }

    $hasBuildableProject = $false
    foreach ($rel in $projectRelPaths) {
      $projPath = Join-Path $Directory.FullName $rel
      if (-not (Test-Path -LiteralPath $projPath -PathType Leaf)) { continue }
      if (-not (Test-IsMakefileVcxproj -VcxprojPath $projPath)) {
        $hasBuildableProject = $true
        break
      }
    }

    if (-not $hasBuildableProject) {
      return $null
    }

    $buildPath = $sln
    $kind = 'sln'
  } else {
    $vcxprojs = @(Get-ChildItem -LiteralPath $Directory.FullName -File -Filter '*.vcxproj')
    if ($vcxprojs.Count -eq 1) {
      if (Test-IsMakefileVcxproj -VcxprojPath $vcxprojs[0].FullName) {
        return $null
      }
      $buildPath = $vcxprojs[0].FullName
      $kind = 'vcxproj'
    } elseif ($vcxprojs.Count -eq 0) {
      return $null
    } else {
      throw "Directory '$($Directory.FullName)' has multiple '*.vcxproj' files but no '$name.sln'."
    }
  }

  # Skip classic WDK NMake wrapper projects/solutions (MakeFileProj / ConfigurationType=Makefile).
  if ($kind -eq 'vcxproj' -and (Test-IsMakefileVcxproj -VcxprojPath $buildPath)) {
    Write-Host ("Skipping driver project because '{0}' is a MakeFileProj/Makefile (legacy WDK build wrapper; not CI-buildable)." -f $buildPath)
    return $null
  }
  if ($kind -eq 'sln' -and (Test-IsMakefileSolution -SolutionPath $buildPath)) {
    Write-Host ("Skipping driver solution because '{0}' references MakeFileProj/Makefile project(s) (legacy WDK build wrapper; not CI-buildable)." -f $buildPath)
    return $null
  }

  # Require an INF in the same directory tree so downstream catalog/sign/package steps can run.
  if (-not (Test-HasInfInTree -DirectoryPath $Directory.FullName)) {
    return $null
  }

  $sep = [IO.Path]::DirectorySeparatorChar
  $altSep = [IO.Path]::AltDirectorySeparatorChar
  $driversRootNormalized = $DriversRootResolved.TrimEnd($sep, $altSep)
  $dirResolved = (Resolve-Path -LiteralPath $Directory.FullName).Path.TrimEnd($sep, $altSep)
  $prefix = $driversRootNormalized + $sep

  if (-not $dirResolved.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Internal error: expected '$dirResolved' to be under '$driversRootNormalized'."
  }

  $relativePath = $dirResolved.Substring($prefix.Length)
  $displayName = $relativePath.Replace($sep, '/')
  if ($altSep -ne $sep) {
    $displayName = $displayName.Replace($altSep, '/')
  }

  return [pscustomobject]@{
    Name = $displayName
    RelativePath = $relativePath
    LeafName = $name
    Kind = $kind
    BuildPath = (Resolve-Path -LiteralPath $buildPath).Path
    Directory = $dirResolved
  }
}

function Discover-DriverBuildTargets {
  param(
    [Parameter(Mandatory = $true)][string]$DriversRoot,
    [string[]]$AllowList
  )

  if (-not (Test-Path -LiteralPath $DriversRoot -PathType Container)) {
    return @()
  }

  $driversRootResolved = (Resolve-Path -LiteralPath $DriversRoot).Path

  $targets = New-Object System.Collections.Generic.List[object]

  # Discover driver roots at any depth under drivers/. A directory is considered a
  # build target if it contains:
  #   - <dirName>.sln, OR
  #   - exactly one *.vcxproj
  #
  # Additionally, we filter to "CI-buildable" driver projects:
  #   - ignore WDK NMake wrapper projects (MakeFileProj / ConfigurationType=Makefile)
  #   - require at least one INF in the directory tree (for catalog/sign/package)
  $buildFiles = @()
  $buildFiles += @(Get-ChildItem -LiteralPath $DriversRoot -Recurse -File -Filter '*.vcxproj' -ErrorAction SilentlyContinue)
  $buildFiles += @(Get-ChildItem -LiteralPath $DriversRoot -Recurse -File -Filter '*.sln' -ErrorAction SilentlyContinue)

  $dirMap = @{}
  foreach ($file in $buildFiles) {
    if (-not $file.Directory) { continue }
    $full = $file.Directory.FullName
    if ([string]::IsNullOrWhiteSpace($full)) { continue }
    $key = $full.ToLowerInvariant()
    if (-not $dirMap.ContainsKey($key)) {
      $dirMap[$key] = $file.Directory
    }
  }

  foreach ($dir in ($dirMap.Values | Sort-Object -Property FullName)) {
    $target = Try-GetDriverBuildTargetFromDirectory -Directory $dir -DriversRootResolved $driversRootResolved
    if ($null -ne $target) {
      [void]$targets.Add($target)
    }
  }

  $allTargets = ,$targets.ToArray()

  if (-not $AllowList -or $AllowList.Count -eq 0) {
    return $allTargets
  }

  $selected = New-Object System.Collections.Generic.List[object]
  $normalizedAllow = Get-UniqueStringsInOrder -Values $AllowList

  foreach ($requestedRaw in $normalizedAllow) {
    $requested = ([string]$requestedRaw).Trim()
    if ([string]::IsNullOrWhiteSpace($requested)) { continue }

    $requestedNorm = $requested.Replace('\', '/')
    $matches = @($allTargets | Where-Object { $_.Name -ieq $requestedNorm })
    if ($matches.Count -eq 0) {
      $matches = @($allTargets | Where-Object { $_.LeafName -ieq $requestedNorm })
    }

    if ($matches.Count -eq 0) {
      throw "Requested driver '$requested' not found under: $DriversRoot"
    }
    if ($matches.Count -gt 1) {
      $ids = ($matches | Select-Object -ExpandProperty Name | Sort-Object -Unique) -join ', '
      throw "Requested driver '$requested' is ambiguous. Matches: $ids"
    }

    [void]$selected.Add($matches[0])
  }

  return ,$selected.ToArray()
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
$logRoot = Join-Path (Join-Path $outRoot 'logs') 'drivers'
$objRoot = Join-Path (Join-Path $outRoot 'obj') 'drivers'

$normalizedPlatforms = @()
foreach ($p in $Platforms) { $normalizedPlatforms += (Normalize-Platform -Platform $p) }
$normalizedPlatforms = Get-UniqueStringsInOrder -Values $normalizedPlatforms

Write-Host "Configuration: $Configuration"
Write-Host ("Platforms: {0}" -f ($normalizedPlatforms -join ', '))
if ($Drivers) { Write-Host ("Drivers allowlist: {0}" -f ($Drivers -join ', ')) }

$toolchain = Read-ToolchainJson -ToolchainJsonPath $ToolchainJson
if ($null -ne $toolchain) {
  $bootstrapPlatform = if ($normalizedPlatforms -contains 'x64') { 'x64' } else { $normalizedPlatforms[0] }
  Initialize-ToolchainEnvironment -Toolchain $toolchain -Platform $bootstrapPlatform
}
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
    if ($null -ne $toolchain) {
      Initialize-ToolchainEnvironment -Toolchain $toolchain -Platform $platform
    }

    $arch = Platform-ToArch -Platform $platform

    $driverOutDir = Join-Path (Join-Path $outDriversRoot $target.RelativePath) $arch
    $driverObjDir = Join-Path (Join-Path $objRoot $target.RelativePath) $arch

    Ensure-EmptyDirectory -Path $driverOutDir
    Ensure-EmptyDirectory -Path $driverObjDir

    $logBase = Get-SafeFileName -Value $target.Name
    $logFile = Join-Path $logRoot ("{0}-{1}.msbuild.log" -f $logBase, $arch)
    $binLogFile = Join-Path $logRoot ("{0}-{1}.msbuild.binlog" -f $logBase, $arch)

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
