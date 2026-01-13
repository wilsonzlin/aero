#Requires -Version 5.1
<#
.SYNOPSIS
  Build AeroGPU Win7 dbgctl tool (aerogpu_dbgctl.exe) for CI packaging.

.DESCRIPTION
  Builds `drivers/aerogpu/tools/win7_dbgctl` using `cl.exe` from a Visual Studio toolchain.
  This tool intentionally does not require the WDK; it dynamically loads the required D3DKMT*
  entrypoints from gdi32.dll (see the tool README).

  Outputs are staged into the driver build output tree so `ci/make-catalogs.ps1` copies them
  into `out/packages/**` and downstream Guest Tools / driver bundle packaging picks them up:

    out/drivers/aerogpu/x86/tools/aerogpu_dbgctl.exe
    out/drivers/aerogpu/x64/tools/aerogpu_dbgctl.exe

.PARAMETER ToolchainJson
  Path to a toolchain manifest produced by `ci/install-wdk.ps1` (default: out/toolchain.json).

.PARAMETER OutDriversRoot
  Output root for driver build outputs (default: out/drivers).

.PARAMETER ObjRoot
  Output root for intermediate object files (default: out/obj/tools).

.PARAMETER Platforms
  Platforms to build (default: Win32 + x64).
#>

[CmdletBinding()]
param(
  [string]$ToolchainJson = "out/toolchain.json",
  [string]$OutDriversRoot = "out/drivers",
  [string]$ObjRoot = "out/obj/tools",
  [string[]]$Platforms = @("Win32", "x64")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoPath {
  param([Parameter(Mandatory = $true)][string]$Path)

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string]$Path)

  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Ensure-Directory {
  param([Parameter(Mandatory = $true)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path)) {
    New-Item -ItemType Directory -Force -Path $Path | Out-Null
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

function Initialize-ToolchainEnvironment {
  param(
    $Toolchain,
    [Parameter(Mandatory = $true)][string]$Platform
  )

  if ($null -eq $Toolchain) { return }

  $vsDevCmd = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VsDevCmd', 'vsDevCmd', 'vsdevcmd')
  $vcVarsAll = Get-ToolchainPropertyValue -Toolchain $Toolchain -Names @('VcVarsAll', 'vcVarsAll', 'vcvarsall')

  # If we have a dev-environment batch file, use it to populate PATH/INCLUDE/LIB/etc.
  # We import an environment per *target* platform so building both Win32 and x64 works reliably.
  if ($vsDevCmd) {
    $hostVsArch = 'x64'
    $procArch = [string]$env:PROCESSOR_ARCHITECTURE
    if ($procArch -and $procArch.Trim().ToLowerInvariant() -eq 'x86') {
      $hostVsArch = 'x86'
    }

    $targetVsArch = if ($Platform -eq 'Win32') { 'x86' } else { 'x64' }
    Write-Host "Importing Visual Studio environment via VsDevCmd: $vsDevCmd (-arch=$targetVsArch -host_arch=$hostVsArch)"
    Import-EnvironmentFromBatchFile -BatchPath $vsDevCmd -Arguments @("-arch=$targetVsArch", "-host_arch=$hostVsArch")
    return
  }

  if ($vcVarsAll) {
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
    return
  }

  Write-Warning "ToolchainJson did not contain VsDevCmd/VcVarsAll; assuming cl.exe is already available in PATH."
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$toolchainJsonAbs = if ($ToolchainJson) { Resolve-RepoPath -Path $ToolchainJson } else { $null }
$toolchain = Read-ToolchainJson -ToolchainJsonPath $toolchainJsonAbs

$srcFile = Join-Path $repoRoot "drivers\aerogpu\tools\win7_dbgctl\src\aerogpu_dbgctl.cpp"
$includeDir = Join-Path $repoRoot "drivers\aerogpu\protocol"

if (-not (Test-Path -LiteralPath $srcFile -PathType Leaf)) {
  throw "AeroGPU dbgctl source file not found: $srcFile"
}
if (-not (Test-Path -LiteralPath $includeDir -PathType Container)) {
  throw "AeroGPU protocol include dir not found: $includeDir"
}

$normalizedPlatforms = @()
foreach ($p in $Platforms) { $normalizedPlatforms += (Normalize-Platform -Platform $p) }
$normalizedPlatforms = @($normalizedPlatforms | Select-Object -Unique)

Write-Host "Building aerogpu_dbgctl.exe for: $($normalizedPlatforms -join ', ')"

foreach ($platform in $normalizedPlatforms) {
  Initialize-ToolchainEnvironment -Toolchain $toolchain -Platform $platform

  $arch = Platform-ToArch -Platform $platform
  $outDir = Resolve-RepoPath -Path (Join-Path $OutDriversRoot (Join-Path "aerogpu\$arch" "tools"))
  $exePath = Join-Path $outDir "aerogpu_dbgctl.exe"

  $objDir = Resolve-RepoPath -Path (Join-Path $ObjRoot (Join-Path "aerogpu_dbgctl" $arch))
  Ensure-EmptyDirectory -Path $objDir
  Ensure-Directory -Path $outDir

  $clCmd = Get-Command -Name "cl.exe" -ErrorAction SilentlyContinue
  if (-not $clCmd) {
    throw "cl.exe not found. Ensure Visual Studio Build Tools are installed and ToolchainJson contains VsDevCmd/VcVarsAll (or run from a VS Developer Command Prompt)."
  }

  Write-Host ""
  Write-Host "==> aerogpu_dbgctl ($arch)"
  Write-Host "    src: $srcFile"
  Write-Host "    out: $exePath"

  Push-Location $objDir
  try {
    $args = @(
      "/nologo",
      "/W4",
      "/EHsc",
      "/O2",
      "/MT",
      "/DUNICODE",
      "/D_UNICODE",
      "/I", $includeDir,
      $srcFile,
      "/link",
      "/OUT:$exePath",
      "user32.lib",
      "gdi32.lib"
    )

    & $clCmd.Source @args
    if ($LASTEXITCODE -ne 0) {
      throw "cl.exe failed for aerogpu_dbgctl ($arch) (exit code $LASTEXITCODE)."
    }
  } finally {
    Pop-Location
  }

  if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) {
    throw "Expected dbgctl output missing after build: $exePath"
  }
}

Write-Host ""
Write-Host "AeroGPU dbgctl build complete."

