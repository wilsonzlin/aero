[CmdletBinding(DefaultParameterSetName = "FromIso")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutIso = (Join-Path $PSScriptRoot "..\out\aero-virtio-win7-drivers.iso"),

  # Optional: override which driver packages are extracted from virtio-win.
  [string[]]$Drivers,

  # Optional: fail if audio/input drivers are requested but missing from virtio-win.
  [switch]$StrictOptional,

  # Delete the staging directory produced by make-driver-pack.ps1 after the ISO is built.
  [switch]$CleanStage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-Python {
  $candidates = @("python", "python3", "py")
  foreach ($c in $candidates) {
    $cmd = Get-Command $c -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
  }
  return $null
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$packScript = Join-Path $PSScriptRoot "make-driver-pack.ps1"
$driversOut = Join-Path $PSScriptRoot "..\out"
$null = New-Item -ItemType Directory -Force -Path $driversOut
$driversOut = (Resolve-Path -LiteralPath $driversOut).Path
$packRoot = Join-Path $driversOut "aero-win7-driver-pack"

if (-not (Test-Path -LiteralPath $packScript -PathType Leaf)) {
  throw "Expected script not found: $packScript"
}

Write-Host "Building Win7 virtio driver staging directory..."

$packArgs = @(
  "-OutDir", $driversOut,
  "-NoZip"
)

if ($PSBoundParameters.ContainsKey("Drivers")) {
  $packArgs += "-Drivers"
  $packArgs += $Drivers
}
if ($StrictOptional) {
  $packArgs += "-StrictOptional"
}

if ($PSCmdlet.ParameterSetName -eq "FromIso") {
  $packArgs += @("-VirtioWinIso", $VirtioWinIso)
} else {
  $packArgs += @("-VirtioWinRoot", $VirtioWinRoot)
}

& powershell -NoProfile -ExecutionPolicy Bypass -File $packScript @packArgs
if ($LASTEXITCODE -ne 0) {
  throw "make-driver-pack.ps1 failed (exit $LASTEXITCODE)."
}

if (-not (Test-Path -LiteralPath $packRoot -PathType Container)) {
  throw "Expected staging directory not found: $packRoot"
}

$python = Resolve-Python
if (-not $python) {
  throw "Python not found on PATH. Install Python 3 and re-run."
}

$isoBuilder = Join-Path $repoRoot "tools\driver-iso\build.py"
if (-not (Test-Path -LiteralPath $isoBuilder -PathType Leaf)) {
  throw "Expected ISO builder not found: $isoBuilder"
}

$outIsoDir = Split-Path -Parent $OutIso
if (-not (Test-Path -LiteralPath $outIsoDir)) {
  New-Item -ItemType Directory -Force -Path $outIsoDir | Out-Null
}

Write-Host "Building ISO..."
Write-Host "  staging: $packRoot"
Write-Host "  output : $OutIso"

& $python $isoBuilder --drivers-root $packRoot --output $OutIso
if ($LASTEXITCODE -ne 0) {
  throw "driver-iso build failed (exit $LASTEXITCODE)."
}

Write-Host "Done."

if ($CleanStage) {
  Write-Host "Cleaning staging directory: $packRoot"
  Remove-Item -LiteralPath $packRoot -Recurse -Force -ErrorAction SilentlyContinue
}
