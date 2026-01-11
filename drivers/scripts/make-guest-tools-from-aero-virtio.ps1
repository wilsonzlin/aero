[CmdletBinding()]
param(
  # Directory containing built aero virtio driver packages.
  #
  # Expected layout:
  #   x86/aerovblk/*.{inf,sys,cat}
  #   x86/aerovnet/*.{inf,sys,cat}
  #   amd64/aerovblk/*.{inf,sys,cat}   (or x64/ instead of amd64/)
  #   amd64/aerovnet/*.{inf,sys,cat}
  [Parameter(Mandatory = $true)]
  [string]$DriverOutDir,

  # Directory containing Guest Tools scripts/config/certs.
  # Defaults to the in-repo `guest-tools/` directory.
  [string]$GuestToolsDir,

  # Output directory for `aero-guest-tools.iso`, `aero-guest-tools.zip`, and `manifest.json`.
  [string]$OutDir,

  [string]$Version = "0.0.0",

  [string]$BuildId = "local"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
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

function Resolve-InputArchDir {
  param(
    [Parameter(Mandatory = $true)][string]$DriversDir,
    [Parameter(Mandatory = $true)][ValidateSet("x86", "amd64")][string]$ArchOut
  )

  # Keep in sync with `tools/packaging/aero_packager/src/lib.rs` (`resolve_input_arch_dir`).
  if ($ArchOut -eq "x86") {
    $candidates = @("x86", "win32", "i386")
  } else {
    $candidates = @("amd64", "x64", "x86_64", "x86-64")
  }

  foreach ($name in $candidates) {
    $p = Join-Path $DriversDir $name
    if (Test-Path -LiteralPath $p -PathType Container) {
      return $p
    }
  }

  $tried = ($candidates | ForEach-Object { (Join-Path $DriversDir $_) }) -join ", "
  throw "drivers dir missing required architecture directory for $ArchOut; tried: $tried"
}

function Require-DriverPackageComplete {
  param(
    [Parameter(Mandatory = $true)][string]$DriverDir,
    [Parameter(Mandatory = $true)][string]$DriverName,
    [Parameter(Mandatory = $true)][string]$Arch
  )

  if (-not (Test-Path -LiteralPath $DriverDir -PathType Container)) {
    throw "Required driver directory missing: $DriverDir"
  }

  $inf = @(Get-ChildItem -LiteralPath $DriverDir -Recurse -File -Filter "*.inf" -ErrorAction SilentlyContinue)
  $sys = @(Get-ChildItem -LiteralPath $DriverDir -Recurse -File -Filter "*.sys" -ErrorAction SilentlyContinue)
  $cat = @(Get-ChildItem -LiteralPath $DriverDir -Recurse -File -Filter "*.cat" -ErrorAction SilentlyContinue)

  if ($inf.Count -eq 0 -or $sys.Count -eq 0 -or $cat.Count -eq 0) {
    throw "Required driver $DriverName ($Arch) is incomplete: expected at least one .inf, .sys, and .cat under $DriverDir"
  }
}

$repoRoot = Resolve-RepoRoot

if (-not $OutDir) {
  $OutDir = Join-Path $repoRoot "dist\guest-tools"
}
if (-not $GuestToolsDir) {
  $GuestToolsDir = Join-Path $repoRoot "guest-tools"
}

$specPath = Join-Path $repoRoot "tools\packaging\specs\win7-aero-virtio.json"

if (-not (Test-Path -LiteralPath $GuestToolsDir -PathType Container)) {
  throw "Expected directory not found: $GuestToolsDir"
}
if (-not (Test-Path -LiteralPath $specPath -PathType Leaf)) {
  throw "Expected packaging spec not found: $specPath"
}

$driversRoot = (Resolve-Path -LiteralPath $DriverOutDir).Path

# Preflight: ensure the required aero driver packages exist for both architectures.
$driversX86Dir = Resolve-InputArchDir -DriversDir $driversRoot -ArchOut "x86"
$driversAmd64Dir = Resolve-InputArchDir -DriversDir $driversRoot -ArchOut "amd64"

foreach ($driver in @("aerovblk", "aerovnet")) {
  Require-DriverPackageComplete -DriverDir (Join-Path $driversX86Dir $driver) -DriverName $driver -Arch "x86"
  Require-DriverPackageComplete -DriverDir (Join-Path $driversAmd64Dir $driver) -DriverName $driver -Arch "amd64"
}

Write-Host "Driver artifacts validated:"
Write-Host "  drivers: $driversRoot"
Write-Host "  guest-tools: $GuestToolsDir"

# Build the actual Guest Tools ISO/zip using the in-repo packager.
Require-Command -Name "cargo" | Out-Null

$packagerManifest = Join-Path $repoRoot "tools\packaging\aero_packager\Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Expected packager manifest not found: $packagerManifest"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools..."
Write-Host "  spec : $specPath"
Write-Host "  out  : $OutDir"

& cargo run --manifest-path $packagerManifest --release -- `
  --drivers-dir $driversRoot `
  --guest-tools-dir $GuestToolsDir `
  --spec $specPath `
  --out-dir $OutDir `
  --version $Version `
  --build-id $BuildId
if ($LASTEXITCODE -ne 0) {
  throw "aero_packager failed (exit $LASTEXITCODE)."
}

Write-Host "Done."
