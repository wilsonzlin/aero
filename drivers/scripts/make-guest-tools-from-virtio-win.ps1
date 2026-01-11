[CmdletBinding(DefaultParameterSetName = "FromIso")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutDir,

  [string]$Version = "0.0.0",

  [string]$BuildId = "local",

  [string]$SpecPath,

  [switch]$CleanStage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
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

function Require-Command {
  param([Parameter(Mandatory = $true)][string]$Name)
  $cmd = Get-Command $Name -ErrorAction SilentlyContinue
  if (-not $cmd) {
    throw "Required tool not found on PATH: $Name"
  }
  return $cmd.Source
}

$repoRoot = Resolve-RepoRoot

if (-not $OutDir) {
  $OutDir = Join-Path $repoRoot "dist\guest-tools"
}
if (-not $SpecPath) {
  $SpecPath = Join-Path $repoRoot "tools\packaging\specs\win7-virtio-win.json"
}

$guestToolsDir = Join-Path $repoRoot "guest-tools"
$packScript = Join-Path $repoRoot "drivers\scripts\make-driver-pack.ps1"
$driversOutDir = Join-Path $repoRoot "drivers\out"
$driverPackRoot = Join-Path $driversOutDir "aero-win7-driver-pack"
$packagerDriversRoot = Join-Path $driversOutDir "aero-guest-tools-drivers"

if (-not (Test-Path -LiteralPath $packScript -PathType Leaf)) {
  throw "Expected script not found: $packScript"
}
if (-not (Test-Path -LiteralPath $guestToolsDir -PathType Container)) {
  throw "Expected directory not found: $guestToolsDir"
}
$guestToolsNotices = Join-Path $guestToolsDir "THIRD_PARTY_NOTICES.md"
if (-not (Test-Path -LiteralPath $guestToolsNotices -PathType Leaf)) {
  throw "Expected third-party notices file not found: $guestToolsNotices"
}
if (-not (Test-Path -LiteralPath $SpecPath -PathType Leaf)) {
  throw "Expected packaging spec not found: $SpecPath"
}

Ensure-Directory -Path $driversOutDir

# 1) Build the standard Win7 driver pack staging directory (from virtio-win ISO/root).
$packArgs = @(
  "-OutDir", $driversOutDir,
  "-NoZip"
)
if ($PSCmdlet.ParameterSetName -eq "FromIso") {
  $packArgs += @("-VirtioWinIso", $VirtioWinIso)
} else {
  $packArgs += @("-VirtioWinRoot", $VirtioWinRoot)
}

Write-Host "Building driver pack staging directory..."
& powershell -NoProfile -ExecutionPolicy Bypass -File $packScript @packArgs
if ($LASTEXITCODE -ne 0) {
  throw "make-driver-pack.ps1 failed (exit $LASTEXITCODE)."
}
if (-not (Test-Path -LiteralPath $driverPackRoot -PathType Container)) {
  throw "Expected driver pack staging directory not found: $driverPackRoot"
}

# 2) Convert the driver pack layout (win7/x86 + win7/amd64) into the Guest Tools packager input layout (x86 + amd64).
Ensure-EmptyDirectory -Path $packagerDriversRoot
Ensure-EmptyDirectory -Path (Join-Path $packagerDriversRoot "x86")
Ensure-EmptyDirectory -Path (Join-Path $packagerDriversRoot "amd64")

function Copy-DriverTree {
  param(
    [Parameter(Mandatory = $true)][string]$SourceArchDir,
    [Parameter(Mandatory = $true)][string]$DestArchDir
  )

  if (-not (Test-Path -LiteralPath $SourceArchDir -PathType Container)) {
    throw "Expected directory not found: $SourceArchDir"
  }

  $children = Get-ChildItem -LiteralPath $SourceArchDir -Directory | Sort-Object -Property Name
  foreach ($d in $children) {
    $dst = Join-Path $DestArchDir $d.Name
    Copy-Item -LiteralPath $d.FullName -Destination $dst -Recurse -Force
  }
}

Copy-DriverTree -SourceArchDir (Join-Path $driverPackRoot "win7\\x86") -DestArchDir (Join-Path $packagerDriversRoot "x86")
Copy-DriverTree -SourceArchDir (Join-Path $driverPackRoot "win7\\amd64") -DestArchDir (Join-Path $packagerDriversRoot "amd64")

Write-Host "Packager input prepared:"
Write-Host "  $packagerDriversRoot"

# 3) Build the actual Guest Tools ISO/zip using the in-repo packager.
Require-Command -Name "cargo" | Out-Null

$packagerManifest = Join-Path $repoRoot "tools\\packaging\\aero_packager\\Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Expected packager manifest not found: $packagerManifest"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools..."
Write-Host "  spec : $SpecPath"
Write-Host "  out  : $OutDir"

& cargo run --manifest-path $packagerManifest --release -- `
  --drivers-dir $packagerDriversRoot `
  --guest-tools-dir $guestToolsDir `
  --spec $SpecPath `
  --out-dir $OutDir `
  --version $Version `
  --build-id $BuildId
if ($LASTEXITCODE -ne 0) {
  throw "aero_packager failed (exit $LASTEXITCODE)."
}

Write-Host "Done."

if ($CleanStage) {
  Write-Host "Cleaning staging directories..."
  Remove-Item -LiteralPath $driverPackRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $packagerDriversRoot -Recurse -Force -ErrorAction SilentlyContinue
}

