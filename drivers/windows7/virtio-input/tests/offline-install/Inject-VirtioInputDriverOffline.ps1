#Requires -Version 5.1
# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Injects (stages) the Aero Windows 7 virtio-input driver into an already-installed offline Windows directory.

.DESCRIPTION
  This script automates the manual "Option B" flow from README.md for CI/harness workflows that operate on
  a prebuilt Windows VHD/VHDX (or any mounted offline Windows installation directory).

  It runs:

    dism /English /Image:<ImagePath> /Add-Driver /Driver:<DriverPackageDir> /Recurse

  Optionally with /ForceUnsigned (test-only), then verifies staging by invoking Verify-VirtioInputStaged.ps1.

.PARAMETER ImagePath
  Path to the offline Windows root directory (must contain a 'Windows\' directory), e.g. W:\ or C:\wim\mount

.PARAMETER DriverPackageDir
  Directory containing aero_virtio_input.inf and aero_virtio_input.sys (and optionally aero_virtio_input.cat)

.PARAMETER ForceUnsigned
  Pass /ForceUnsigned to DISM /Add-Driver.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\Inject-VirtioInputDriverOffline.ps1 `
    -ImagePath W:\ `
    -DriverPackageDir C:\src\aero\out\packages\windows7\virtio-input\x64
#>

[CmdletBinding()]
param(
  # Offline Windows root directory (must contain Windows\).
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$ImagePath,

  # Directory containing aero_virtio_input.inf/.sys (and optionally .cat).
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$DriverPackageDir,

  # Pass /ForceUnsigned to DISM /Add-Driver.
  [switch]$ForceUnsigned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-IsAdministrator {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated PowerShell prompt (Run as Administrator)."
  }
}

function Assert-CommandAvailable {
  param([Parameter(Mandatory)][string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command '$Name' was not found in PATH."
  }
}

function Normalize-Path {
  param([Parameter(Mandatory)][string]$Path)

  $expanded = [Environment]::ExpandEnvironmentVariables($Path)

  # DISM examples use W:\ (drive root). Accept "W:" and normalize it.
  if ($expanded -match "^[A-Za-z]:$") {
    $expanded = "$expanded\"
  }

  try {
    return (Resolve-Path -LiteralPath $expanded -ErrorAction Stop).Path
  }
  catch {
    return $expanded
  }
}

function Format-Arg {
  param([Parameter(Mandatory)][string]$Arg)
  if ($Arg -match '[\s"`]') {
    return '"' + ($Arg -replace '"', '\"') + '"'
  }
  return $Arg
}

function Invoke-NativeCommandResult {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $cmdLine = ("{0} {1}" -f $FilePath, (($ArgumentList | ForEach-Object { Format-Arg $_ }) -join " ")).Trim()
  Write-Host "`n> $cmdLine"

  # Avoid reporting stale exit codes from previous native commands.
  $global:LASTEXITCODE = 0
  $output = & $FilePath @ArgumentList 2>&1

  if (-not $SuppressOutput) {
    foreach ($line in $output) {
      Write-Host $line
    }
  }

  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ,$output
    CommandLine = $cmdLine
  }
}

function Invoke-NativeCommand {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $result = Invoke-NativeCommandResult -FilePath $FilePath -ArgumentList $ArgumentList -SuppressOutput:$SuppressOutput
  if ($result.ExitCode -ne 0) {
    $outputText = ($result.Output | Out-String).Trim()
    if ($outputText) {
      throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)`n`n$outputText"
    }
    throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)"
  }
}

Assert-IsAdministrator
Assert-CommandAvailable -Name "dism.exe"
Assert-CommandAvailable -Name "powershell.exe"

$resolvedImagePath = Normalize-Path -Path $ImagePath
$resolvedDriverPackageDir = Normalize-Path -Path $DriverPackageDir

if (-not (Test-Path -LiteralPath $resolvedImagePath -PathType Container)) {
  throw "-ImagePath must be an existing directory. Got: $resolvedImagePath"
}
if (-not (Test-Path -LiteralPath $resolvedDriverPackageDir -PathType Container)) {
  throw "-DriverPackageDir must be an existing directory. Got: $resolvedDriverPackageDir"
}

$resolvedDriverPackageDir = (Resolve-Path -LiteralPath $resolvedDriverPackageDir).Path

$windowsDir = Join-Path -Path $resolvedImagePath -ChildPath "Windows"
if (-not (Test-Path -LiteralPath $windowsDir -PathType Container)) {
  throw ("-ImagePath does not look like an offline Windows root (missing 'Windows\' directory).`n" +
    "ImagePath: $resolvedImagePath`n" +
    "Expected:  $windowsDir")
}

$infPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.inf"
$sysPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.sys"
$catPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.cat"

if (-not (Test-Path -LiteralPath $infPath -PathType Leaf)) {
  throw ("-DriverPackageDir must contain 'aero_virtio_input.inf'.`n" +
    "Expected: $infPath")
}
if (-not (Test-Path -LiteralPath $sysPath -PathType Leaf)) {
  throw ("-DriverPackageDir must contain 'aero_virtio_input.sys'.`n" +
    "Expected: $sysPath")
}
if (-not (Test-Path -LiteralPath $catPath -PathType Leaf)) {
  if (-not $ForceUnsigned) {
    Write-Warning "Driver package is missing aero_virtio_input.cat. DISM may reject the package unless you use -ForceUnsigned (test-only)."
  }
}

Write-Host "========================================"
Write-Host "virtio-input DISM injection plan (offline OS dir)"
Write-Host "========================================"
Write-Host "ImagePath            : $resolvedImagePath"
Write-Host "DriverPackageDir     : $resolvedDriverPackageDir"
Write-Host "ForceUnsigned        : $(if ($ForceUnsigned) { "ON" } else { "OFF" })"
Write-Host "========================================"

Write-Host "`nAdding driver to offline Windows installation..."
$addArgs = @(
  "/English",
  ("/Image:$resolvedImagePath"),
  "/Add-Driver",
  ("/Driver:$resolvedDriverPackageDir"),
  "/Recurse"
)
if ($ForceUnsigned) {
  $addArgs += "/ForceUnsigned"
}
Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList $addArgs

$verifierScript = Join-Path -Path $PSScriptRoot -ChildPath "Verify-VirtioInputStaged.ps1"
if (-not (Test-Path -LiteralPath $verifierScript -PathType Leaf)) {
  throw "Verifier script not found: $verifierScript"
}

Write-Host "`nVerifying driver is staged..."
$verifyResult = Invoke-NativeCommandResult -FilePath "powershell.exe" -ArgumentList @(
  "-NoProfile",
  "-ExecutionPolicy",
  "Bypass",
  "-File",
  $verifierScript,
  "-ImagePath",
  $resolvedImagePath
)

if ($verifyResult.ExitCode -ne 0) {
  exit $verifyResult.ExitCode
}

Write-Host "`nDone."
