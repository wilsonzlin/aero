[CmdletBinding()]
param(
  # Directory containing built aero virtio driver packages.
  #
  # Expected layout:
  #   x86/aero_virtio_blk/*.{inf,sys,cat}
  #   x86/aero_virtio_net/*.{inf,sys,cat}
  #   amd64/aero_virtio_blk/*.{inf,sys,cat}   (or x64/ instead of amd64/)
  #   amd64/aero_virtio_net/*.{inf,sys,cat}
  [Parameter(Mandatory = $true)]
  [string]$DriverOutDir,

  # Directory containing Guest Tools scripts/config/certs.
  # Defaults to the in-repo `guest-tools/` directory.
  [string]$GuestToolsDir,

  # Output directory for `aero-guest-tools.iso`, `aero-guest-tools.zip`, and `manifest.json`.
  [string]$OutDir,

  [string]$Version = "0.0.0",

  [string]$BuildId = "local",

  # Driver signing / boot policy embedded in Guest Tools manifest.json.
  #
  # - test: media is intended for test-signed/custom-signed drivers (default)
  # - production: media is intended for WHQL/production-signed drivers (no cert injection)
  # - none: same as production (development use)
  #
  # Legacy aliases accepted:
  # - testsigning / test-signing -> test
  # - nointegritychecks / no-integrity-checks -> none
  # - prod / whql -> production
  [ValidateSet("test", "production", "none", "testsigning", "test-signing", "nointegritychecks", "no-integrity-checks", "prod", "whql")]
  [string]$SigningPolicy = "test",

  # Public certificate to embed under Guest Tools `certs/` when SigningPolicy resolves to `test`.
  # If omitted, the script uses whatever certificate(s) already exist under GuestToolsDir\certs\.
  [string]$CertPath,

  # Override SOURCE_DATE_EPOCH for deterministic timestamps inside the ISO/zip.
  [Nullable[long]]$SourceDateEpoch
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

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Copy-DirectoryContents {
  param(
    [Parameter(Mandatory = $true)][string]$SourceDir,
    [Parameter(Mandatory = $true)][string]$DestDir
  )

  if (-not (Test-Path -LiteralPath $SourceDir -PathType Container)) {
    throw "Expected directory not found: $SourceDir"
  }

  Ensure-Directory -Path $DestDir
  $children = Get-ChildItem -LiteralPath $SourceDir | Sort-Object -Property Name
  foreach ($c in $children) {
    $dst = Join-Path $DestDir $c.Name
    Copy-Item -LiteralPath $c.FullName -Destination $dst -Recurse -Force
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

function Normalize-SigningPolicy {
  param([Parameter(Mandatory = $true)][string]$Policy)
  $p = $Policy.Trim().ToLowerInvariant()
  switch ($p) {
    "testsigning" { return "test" }
    "test-signing" { return "test" }
    "nointegritychecks" { return "none" }
    "no-integrity-checks" { return "none" }
    "prod" { return "production" }
    "whql" { return "production" }
    default { return $p }
  }
}

$SigningPolicy = Normalize-SigningPolicy -Policy $SigningPolicy

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
if ($CertPath) {
  $CertPath = (Resolve-Path -LiteralPath $CertPath).Path
}

# Preflight: ensure the required aero driver packages exist for both architectures.
$driversX86Dir = Resolve-InputArchDir -DriversDir $driversRoot -ArchOut "x86"
$driversAmd64Dir = Resolve-InputArchDir -DriversDir $driversRoot -ArchOut "amd64"

foreach ($driver in @("aero_virtio_blk", "aero_virtio_net")) {
  Require-DriverPackageComplete -DriverDir (Join-Path $driversX86Dir $driver) -DriverName $driver -Arch "x86"
  Require-DriverPackageComplete -DriverDir (Join-Path $driversAmd64Dir $driver) -DriverName $driver -Arch "amd64"
}

Write-Host "Driver artifacts validated:"
Write-Host "  drivers: $driversRoot"
Write-Host "  guest-tools: $GuestToolsDir"
Write-Host "  signing policy: $SigningPolicy"

# Build the actual Guest Tools ISO/zip using the in-repo packager.
Require-Command -Name "cargo" | Out-Null

$packagerManifest = Join-Path $repoRoot "tools\packaging\aero_packager\Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Expected packager manifest not found: $packagerManifest"
}

$deviceContractPath = Join-Path $repoRoot "docs\windows-device-contract.json"
if (-not (Test-Path -LiteralPath $deviceContractPath -PathType Leaf)) {
  throw "Expected Windows device contract not found: $deviceContractPath"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools..."
Write-Host "  spec : $specPath"
Write-Host "  out  : $OutDir"
Write-Host "  contract : $deviceContractPath"

Write-Host "Staging Guest Tools input..."
$stageRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("aerogt_aero_virtio_" + [System.Guid]::NewGuid().ToString("N"))
$stageGuestTools = Join-Path $stageRoot "guest-tools"
$success = $false

try {
  Ensure-EmptyDirectory -Path $stageRoot
  Ensure-EmptyDirectory -Path $stageGuestTools
  Copy-DirectoryContents -SourceDir $GuestToolsDir -DestDir $stageGuestTools

  # For WHQL/production-signed driver bundles (`signing_policy=production|none`), do not ship/install
  # any custom root certificates by default.
  if ($SigningPolicy -ne "test") {
    $certsDir = Join-Path $stageGuestTools "certs"
    if (Test-Path -LiteralPath $certsDir -PathType Container) {
      Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
        Remove-Item -Force -ErrorAction SilentlyContinue
    }
  } elseif ($CertPath) {
    $certsDir = Join-Path $stageGuestTools "certs"
    Ensure-Directory -Path $certsDir
    # Replace any existing certificates so the packaged media matches the supplied CertPath.
    Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
      Remove-Item -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $CertPath -Destination $certsDir -Force
  }

  $packagerArgs = @(
    "run",
    "--manifest-path", $packagerManifest,
    "--release",
    "--locked",
    "--",
    "--drivers-dir", $driversRoot,
    "--guest-tools-dir", $stageGuestTools,
    "--spec", $specPath,
    "--windows-device-contract", $deviceContractPath,
    "--out-dir", $OutDir,
    "--version", $Version,
    "--build-id", $BuildId,
    "--signing-policy", $SigningPolicy
  )
  if ($null -ne $SourceDateEpoch) {
    $packagerArgs += @("--source-date-epoch", $SourceDateEpoch)
  }

  & cargo @packagerArgs
  if ($LASTEXITCODE -ne 0) {
    throw "aero_packager failed (exit $LASTEXITCODE)."
  }

  $success = $true
} finally {
  if (Test-Path -LiteralPath $stageRoot) {
    # Always clean up staging directories; Guest Tools outputs are written to -OutDir.
    Remove-Item -LiteralPath $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

Write-Host "Done."
