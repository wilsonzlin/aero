[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Path $MyInvocation.MyCommand.Path -Parent
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir '..')).Path
$outDir = Join-Path $repoRoot 'out'
$outFile = Join-Path $outDir 'toolchain.json'

Import-Module (Join-Path $scriptDir 'lib\Toolchain.psm1') -Force

Write-ToolchainLog -Message "Provisioning Windows driver toolchain (repoRoot=$repoRoot)"

if (-not (Test-Path -LiteralPath $outDir)) {
  New-Item -Path $outDir -ItemType Directory | Out-Null
}

$msbuildExe = Get-MSBuildExe
Write-ToolchainLog -Message "Resolved msbuild.exe: $msbuildExe"

$vsDevCmd = Get-VsDevCmdBat
$vcVarsAll = Get-VcVarsAllBat
if ($null -ne $vsDevCmd -and -not [string]::IsNullOrWhiteSpace($vsDevCmd)) {
  Write-ToolchainLog -Message "Resolved VsDevCmd.bat: $vsDevCmd"
} elseif ($null -ne $vcVarsAll -and -not [string]::IsNullOrWhiteSpace($vcVarsAll)) {
  Write-ToolchainLog -Message "Resolved vcvarsall.bat: $vcVarsAll"
} else {
  Write-ToolchainLog -Level WARN -Message 'Visual Studio developer environment batch files (VsDevCmd.bat / vcvarsall.bat) were not found. MSBuild may still work, but if driver builds fail to locate CL/Link, install Visual Studio Build Tools.'
}

$kitToolchain = Ensure-WindowsKitToolchain -PreferredWdkWingetId 'Microsoft.WindowsWDK' -PreferredWdkKitVersion '10.0.22621.0'

if ($null -eq $kitToolchain.StampInfExe -or [string]::IsNullOrWhiteSpace($kitToolchain.StampInfExe)) {
  Write-ToolchainLog -Level WARN -Message 'stampinf.exe was not found. This tool is optional, but strongly recommended for driver packaging.'
} else {
  Write-ToolchainLog -Message "Resolved stampinf.exe: $($kitToolchain.StampInfExe)"
}

Write-ToolchainLog -Message "Resolved Inf2Cat.exe: $($kitToolchain.Inf2CatExe)"
Write-ToolchainLog -Message "Resolved signtool.exe: $($kitToolchain.SignToolExe)"
Write-ToolchainLog -Message "Inf2Cat.exe kit source: $($kitToolchain.WindowsKits.Inf2Cat.KitFamily) $($kitToolchain.WindowsKits.Inf2Cat.KitToolVersion) ($($kitToolchain.WindowsKits.Inf2Cat.KitBinSource)) @ $($kitToolchain.WindowsKits.Inf2Cat.KitBinDir)"
Write-ToolchainLog -Message "signtool.exe kit source: $($kitToolchain.WindowsKits.SignTool.KitFamily) $($kitToolchain.WindowsKits.SignTool.KitToolVersion) ($($kitToolchain.WindowsKits.SignTool.KitBinSource)) @ $($kitToolchain.WindowsKits.SignTool.KitBinDir)"
if ($null -ne $kitToolchain.WindowsKits.StampInf) {
  Write-ToolchainLog -Message "stampinf.exe kit source: $($kitToolchain.WindowsKits.StampInf.KitFamily) $($kitToolchain.WindowsKits.StampInf.KitToolVersion) ($($kitToolchain.WindowsKits.StampInf.KitBinSource)) @ $($kitToolchain.WindowsKits.StampInf.KitBinDir)"
}

# Make the chosen tool versions resolvable by name for the remainder of this session and (on CI) later steps.
Add-PathEntry -Directory (Split-Path -Path $msbuildExe -Parent)
Add-PathEntry -Directory (Split-Path -Path $kitToolchain.Inf2CatExe -Parent)
Add-PathEntry -Directory (Split-Path -Path $kitToolchain.SignToolExe -Parent)
if ($null -ne $kitToolchain.StampInfExe -and -not [string]::IsNullOrWhiteSpace($kitToolchain.StampInfExe)) {
  Add-PathEntry -Directory (Split-Path -Path $kitToolchain.StampInfExe -Parent)
}

$toolchain = [pscustomobject]@{
  # Common property names expected by other CI scripts.
  MSBuild = $msbuildExe
  MSBuildPath = $msbuildExe
  Inf2Cat = $kitToolchain.Inf2CatExe
  Inf2CatPath = $kitToolchain.Inf2CatExe
  SignTool = $kitToolchain.SignToolExe
  SignToolPath = $kitToolchain.SignToolExe
  StampInf = $kitToolchain.StampInfExe
  StampInfPath = $kitToolchain.StampInfExe
  VsDevCmd = $vsDevCmd
  VcVarsAll = $vcVarsAll

  # Backward/explicit names.
  MSBuildExe = $msbuildExe
  Inf2CatExe = $kitToolchain.Inf2CatExe
  SignToolExe = $kitToolchain.SignToolExe
  StampInfExe = $kitToolchain.StampInfExe
  WindowsKits = $kitToolchain.WindowsKits
  ToolchainJson = $outFile
}

$toolchainJson = $toolchain | ConvertTo-Json -Depth 6
Set-Content -LiteralPath $outFile -Value $toolchainJson -Encoding UTF8
Write-ToolchainLog -Message "Wrote toolchain manifest: $outFile"

Publish-ToolchainToGitHubActions -Toolchain $toolchain

# Final sanity check: ensure the tools can be resolved by name (after PATH adjustment).
Get-Command msbuild.exe -ErrorAction Stop | Out-Null
Get-Command Inf2Cat.exe -ErrorAction Stop | Out-Null
Get-Command signtool.exe -ErrorAction Stop | Out-Null
if ($null -ne $kitToolchain.StampInfExe -and -not [string]::IsNullOrWhiteSpace($kitToolchain.StampInfExe)) {
  Get-Command stampinf.exe -ErrorAction Stop | Out-Null
}

Write-ToolchainLog -Message 'Toolchain provisioning complete.'

