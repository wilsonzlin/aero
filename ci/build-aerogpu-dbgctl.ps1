#Requires -Version 5.1

<#
.SYNOPSIS
  Builds the Windows 7 AeroGPU debug/control utility (aerogpu_dbgctl.exe).

.DESCRIPTION
  The Win7 AeroGPU dbgctl tool is built via the in-tree batch file:
    drivers/aerogpu/tools/win7_dbgctl/build_vs2010.cmd

  This script loads a toolchain manifest (default: out/toolchain.json) produced by
  `ci/install-wdk.ps1`, imports a Visual Studio developer environment for an x86 target so
  `cl.exe` is available, invokes the build script, and verifies the expected output exists:
    drivers/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe

  For CI/local packaging runs, if `out/drivers/aerogpu/<arch>/` exists (i.e. after
  `ci/build-drivers.ps1`), the tool is copied into:
    out/drivers/aerogpu/x86/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe
    out/drivers/aerogpu/x64/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe

  so downstream `ci/make-catalogs.ps1` stages it into `out/packages/**` and Guest Tools / driver
  bundle packaging can ship it.

.PARAMETER ToolchainJson
  Path to a toolchain manifest produced by `ci/install-wdk.ps1` (default: out/toolchain.json).
#>

[CmdletBinding()]
param(
  [Parameter()]
  [ValidateNotNullOrEmpty()]
  [string]$ToolchainJson = 'out/toolchain.json'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-RepoRoot {
  $ciDir = $PSScriptRoot
  if ([string]::IsNullOrWhiteSpace($ciDir)) {
    throw "Unable to determine script directory (PSScriptRoot is empty)."
  }
  return (Resolve-Path -LiteralPath (Join-Path $ciDir '..')).Path
}

function Resolve-RepoPath {
  param([Parameter(Mandatory = $true)][string]$Path)

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  $repoRoot = Resolve-RepoRoot
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Read-ToolchainJson {
  param([Parameter(Mandatory = $true)][string]$ToolchainJsonPath)

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
  # Suppress the batch file's normal output so `set` is the only emitted stdout (stable parsing).
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

function Initialize-VcEnvironmentX86 {
  param(
    [Parameter(Mandatory = $true)]$Toolchain,
    [Parameter(Mandatory = $true)][string]$ToolchainJsonPath
  )

  $vsDevCmdRaw = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VsDevCmd', 'vsDevCmd', 'vsdevcmd')
  $vcVarsAllRaw = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VcVarsAll', 'vcVarsAll', 'vcvarsall')

  $vsDevCmd = if ($vsDevCmdRaw) { [Environment]::ExpandEnvironmentVariables($vsDevCmdRaw).Trim() } else { $null }
  $vcVarsAll = if ($vcVarsAllRaw) { [Environment]::ExpandEnvironmentVariables($vcVarsAllRaw).Trim() } else { $null }

  if (-not [string]::IsNullOrWhiteSpace($vsDevCmd) -and (Test-Path -LiteralPath $vsDevCmd -PathType Leaf)) {
    $args = @('-arch=x86', '-host_arch=x64')
    Write-Host "Importing Visual Studio environment via VsDevCmd.bat: $vsDevCmd ($($args -join ' '))"
    Import-EnvironmentFromBatchFile -BatchPath $vsDevCmd -Arguments $args
    return [pscustomobject]@{
      Kind = 'VsDevCmd'
      Path = $vsDevCmd
      Arguments = $args
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($vsDevCmd)) {
    throw @"
ToolchainJson provides VsDevCmd='$vsDevCmdRaw', but the path does not exist:
  $vsDevCmd

Remediation:
  - Re-run: pwsh -NoProfile -ExecutionPolicy Bypass -File ci/install-wdk.ps1
  - Ensure Visual Studio Build Tools (MSVC C++ toolchain) are installed.
  - Inspect: $ToolchainJsonPath
"@
  }

  if (-not [string]::IsNullOrWhiteSpace($vcVarsAll) -and (Test-Path -LiteralPath $vcVarsAll -PathType Leaf)) {
    $procArch = ([string]$env:PROCESSOR_ARCHITECTURE).Trim().ToLowerInvariant()
    $hostVcArch = if ($procArch -eq 'x86') { 'x86' } else { 'amd64' }
    $archArg = if ($hostVcArch -eq 'amd64') { 'amd64_x86' } else { 'x86' }

    Write-Host "Importing Visual Studio environment via vcvarsall.bat: $vcVarsAll ($archArg)"
    Import-EnvironmentFromBatchFile -BatchPath $vcVarsAll -Arguments @($archArg)
    return [pscustomobject]@{
      Kind = 'VcVarsAll'
      Path = $vcVarsAll
      Arguments = @($archArg)
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($vcVarsAll)) {
    throw @"
ToolchainJson provides VcVarsAll='$vcVarsAllRaw', but the path does not exist:
  $vcVarsAll

Remediation:
  - Re-run: pwsh -NoProfile -ExecutionPolicy Bypass -File ci/install-wdk.ps1
  - Ensure Visual Studio Build Tools (MSVC C++ toolchain) are installed.
  - Inspect: $ToolchainJsonPath
"@
  }

  throw @"
No Visual Studio developer environment batch file was found in ToolchainJson.

Expected one of:
  - VsDevCmd (preferred): <VS>\Common7\Tools\VsDevCmd.bat
  - VcVarsAll:            <VS>\VC\Auxiliary\Build\vcvarsall.bat

Toolchain manifest:
  $ToolchainJsonPath

Remediation:
  - Run: pwsh -NoProfile -ExecutionPolicy Bypass -File ci/install-wdk.ps1
  - Ensure Visual Studio Build Tools are installed with the MSVC C++ toolchain.
"@
}

function Get-PeCoffMachine {
  <#
  .SYNOPSIS
    Returns the PE/COFF Machine field for a PE executable.

  .DESCRIPTION
    Performs minimal PE validation:
      - DOS header magic: "MZ"
      - PE signature: "PE\0\0"
    and then reads the COFF Machine field from the File Header.

    Pure PowerShell/.NET implementation (no external tools). Compatible with Windows PowerShell 5.1.
  #>

  [CmdletBinding()]
  param([Parameter(Mandatory = $true)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "File not found: $Path"
  }

  $absPath = (Resolve-Path -LiteralPath $Path).Path

  $fs = $null
  $br = $null
  try {
    $fs = [System.IO.File]::Open($absPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    if ($fs.Length -lt 64) {
      throw "File is too small to be a valid PE executable (expected at least 64 bytes): $absPath"
    }

    $br = New-Object System.IO.BinaryReader($fs)

    # DOS header: e_magic (0x00) should be 'MZ' (0x5A4D).
    $fs.Position = 0
    $mz = $br.ReadUInt16()
    if ($mz -ne 0x5A4D) {
      throw ("File is not a PE executable (missing 'MZ' DOS header magic): {0}" -f $absPath)
    }

    # DOS header: e_lfanew (0x3C) is a 32-bit offset to the PE signature.
    $fs.Position = 0x3C
    $peOffset = $br.ReadInt32()
    if ($peOffset -lt 0 -or $peOffset -gt ($fs.Length - 6)) {
      throw ("File has an invalid PE header offset (e_lfanew=0x{0:X8}): {1}" -f $peOffset, $absPath)
    }

    # PE signature: 'PE\0\0' (0x00004550).
    $fs.Position = $peOffset
    $peSig = $br.ReadUInt32()
    if ($peSig -ne 0x00004550) {
      throw ("File is not a PE executable (missing 'PE\0\0' signature at offset 0x{0:X8}): {1}" -f $peOffset, $absPath)
    }

    # COFF File Header follows the 4-byte PE signature; Machine is the first field (2 bytes).
    $machine = $br.ReadUInt16()
    return $machine
  } finally {
    if ($br) { $br.Dispose() }
    if ($fs) { $fs.Dispose() }
  }
}

function Assert-PeMachineI386 {
  [CmdletBinding()]
  param([Parameter(Mandatory = $true)][string]$Path)

  $machine = Get-PeCoffMachine -Path $Path

  # IMAGE_FILE_MACHINE_I386 = 0x014c (x86 / 32-bit). The dbgctl tool is intentionally shipped as x86
  # and used on x64 via WOW64.
  if ($machine -ne 0x014c) {
    $absPath = (Resolve-Path -LiteralPath $Path).Path
    throw ("AeroGPU dbgctl must be an x86 (32-bit) PE executable (IMAGE_FILE_MACHINE_I386 0x014c), but '{0}' has COFF Machine 0x{1:X4}. Ensure the dbgctl build uses the x86 toolchain/target, then re-run ci/build-aerogpu-dbgctl.ps1." -f $absPath, $machine)
  }
}

$repoRoot = Resolve-RepoRoot
$toolchainJsonAbs = Resolve-RepoPath -Path $ToolchainJson
$toolchain = Read-ToolchainJson -ToolchainJsonPath $toolchainJsonAbs

$envInfo = Initialize-VcEnvironmentX86 -Toolchain $toolchain -ToolchainJsonPath $toolchainJsonAbs

$cl = Get-Command cl.exe -ErrorAction SilentlyContinue
if (-not $cl) {
  throw @"
cl.exe was not found on PATH after importing the Visual Studio environment.

Environment import:
  $($envInfo.Kind): $($envInfo.Path)
  args: $($envInfo.Arguments -join ' ')

Remediation:
  - Ensure Visual Studio Build Tools are installed with the MSVC C++ toolchain (Desktop development with C++).
  - Re-run: pwsh -NoProfile -ExecutionPolicy Bypass -File ci/install-wdk.ps1
  - Inspect: $toolchainJsonAbs
"@
}

Write-Host "Using cl.exe: $($cl.Source)"

$buildCmd = Join-Path $repoRoot 'drivers\aerogpu\tools\win7_dbgctl\build_vs2010.cmd'
if (-not (Test-Path -LiteralPath $buildCmd -PathType Leaf)) {
  throw "Expected dbgctl build script not found: $buildCmd"
}

Write-Host "Building AeroGPU dbgctl tool..."
& $buildCmd
if ($LASTEXITCODE -ne 0) {
  throw "Dbgctl build script failed with exit code $LASTEXITCODE: $buildCmd"
}

$dbgctlExe = Join-Path $repoRoot 'drivers\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe'
if (-not (Test-Path -LiteralPath $dbgctlExe -PathType Leaf)) {
  throw "Expected dbgctl output was not produced: $dbgctlExe"
}
$dbgctlFile = Get-Item -LiteralPath $dbgctlExe
if ($dbgctlFile.Length -le 0) {
  throw "Dbgctl output exists but is empty: $dbgctlExe"
}

Assert-PeMachineI386 -Path $dbgctlExe

Write-Host ("OK: built dbgctl: {0} ({1} bytes)" -f $dbgctlExe, $dbgctlFile.Length)

# Best-effort staging into the driver build output directories so downstream `ci/make-catalogs.ps1`
# (and thus driver packages + Guest Tools) can include the tool.
foreach ($arch in @('x86', 'x64')) {
  $driverOutDir = Join-Path $repoRoot (Join-Path 'out\drivers\aerogpu' $arch)
  if (-not (Test-Path -LiteralPath $driverOutDir -PathType Container)) {
    Write-Host "NOTE: not staging dbgctl into '$driverOutDir' (directory not found). Run ci/build-drivers.ps1 first to produce out/drivers."
    continue
  }

  $toolsDir = Join-Path $driverOutDir 'tools'
  $dbgctlBinDir = Join-Path $toolsDir (Join-Path 'win7_dbgctl' 'bin')
  New-Item -ItemType Directory -Force -Path $dbgctlBinDir | Out-Null

  $dest = Join-Path $dbgctlBinDir 'aerogpu_dbgctl.exe'
  Copy-Item -LiteralPath $dbgctlExe -Destination $dest -Force
  $destFile = Get-Item -LiteralPath $dest
  Write-Host ("Staged dbgctl for packaging: {0} ({1} bytes)" -f $destFile.FullName, $destFile.Length)
}

