[CmdletBinding(DefaultParameterSetName = "FromIso")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutDir,

  [string]$Version = "0.0.0",

  [string]$BuildId = "local",

  [ValidateSet("none", "testsigning", "nointegritychecks")]
  [string]$SigningPolicy = "none",

  # Optional: override which driver packages are extracted from virtio-win.
  [string[]]$Drivers,

  # Optional: fail if audio/input drivers are requested but missing from virtio-win.
  [switch]$StrictOptional,

  [string]$SpecPath,

  [switch]$CleanStage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path (Join-Path $PSScriptRoot "..") "..")).Path
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
  $OutDir = Join-Path (Join-Path $repoRoot "dist") "guest-tools"
}
if (-not $SpecPath) {
  $SpecPath = Join-Path (Join-Path (Join-Path (Join-Path $repoRoot "tools") "packaging") "specs") "win7-virtio-win.json"
}

$guestToolsDir = Join-Path $repoRoot "guest-tools"
$packScript = Join-Path (Join-Path (Join-Path $repoRoot "drivers") "scripts") "make-driver-pack.ps1"
$driversOutDir = Join-Path (Join-Path $repoRoot "drivers") "out"
$driverPackRoot = Join-Path $driversOutDir "aero-win7-driver-pack"
$packagerDriversRoot = Join-Path $driversOutDir "aero-guest-tools-drivers"
$guestToolsStageDir = Join-Path $driversOutDir "aero-guest-tools-stage"

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

Write-Host "Building driver pack staging directory..."
if ($PSCmdlet.ParameterSetName -eq "FromIso") {
  $winPs = Get-Command "powershell" -ErrorAction SilentlyContinue
  if ($winPs) {
    & $winPs.Source -NoProfile -ExecutionPolicy Bypass -File $packScript @packArgs
  } else {
    & $packScript @packArgs
  }
} else {
  & $packScript @packArgs
}
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

$driverPackWin7Root = Join-Path $driverPackRoot "win7"
Copy-DriverTree -SourceArchDir (Join-Path $driverPackWin7Root "x86") -DestArchDir (Join-Path $packagerDriversRoot "x86")
Copy-DriverTree -SourceArchDir (Join-Path $driverPackWin7Root "amd64") -DestArchDir (Join-Path $packagerDriversRoot "amd64")

Write-Host "Packager input prepared:"
Write-Host "  $packagerDriversRoot"

# 3) Stage the Guest Tools directory so we can inject upstream attribution files
# (without modifying the in-repo guest-tools/ directory).
Ensure-EmptyDirectory -Path $guestToolsStageDir
Copy-Item -LiteralPath (Join-Path $guestToolsDir "*") -Destination $guestToolsStageDir -Recurse -Force

$driverPackLicenses = Join-Path $driverPackRoot "licenses"
$stageLicenses = Join-Path $guestToolsStageDir "licenses"
if (Test-Path -LiteralPath $stageLicenses) {
  Remove-Item -LiteralPath $stageLicenses -Recurse -Force -ErrorAction SilentlyContinue
}
if (Test-Path -LiteralPath $driverPackLicenses -PathType Container) {
  Copy-Item -LiteralPath $driverPackLicenses -Destination $guestToolsStageDir -Recurse -Force
}

$driverPackManifest = Join-Path $driverPackRoot "manifest.json"
if (Test-Path -LiteralPath $driverPackManifest -PathType Leaf) {
  $virtioLicensesDir = Join-Path (Join-Path $guestToolsStageDir "licenses") "virtio-win"
  Ensure-Directory -Path $virtioLicensesDir
  Copy-Item -LiteralPath $driverPackManifest -Destination (Join-Path $virtioLicensesDir "driver-pack-manifest.json") -Force
}

$guestToolsNoticesStage = Join-Path $guestToolsStageDir "THIRD_PARTY_NOTICES.md"
if (-not (Test-Path -LiteralPath $guestToolsNoticesStage -PathType Leaf)) {
  throw "Expected third-party notices file not found after staging: $guestToolsNoticesStage"
}

Write-Host "Guest Tools input staged:"
Write-Host "  $guestToolsStageDir"

# For WHQL/production-signed driver bundles (virtio-win), do not ship/install any
# custom root certificates by default.
if ($SigningPolicy -eq "none") {
  $certsDir = Join-Path $guestToolsStageDir "certs"
  if (Test-Path -LiteralPath $certsDir -PathType Container) {
    Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
      Remove-Item -Force -ErrorAction SilentlyContinue
  }
}

# 4) Build the actual Guest Tools ISO/zip using the in-repo packager.
Require-Command -Name "cargo" | Out-Null

$packagerManifest = Join-Path (Join-Path (Join-Path (Join-Path $repoRoot "tools") "packaging") "aero_packager") "Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Expected packager manifest not found: $packagerManifest"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools..."
Write-Host "  spec : $SpecPath"
Write-Host "  out  : $OutDir"

& cargo run --manifest-path $packagerManifest --release --locked -- `
  --drivers-dir $packagerDriversRoot `
  --guest-tools-dir $guestToolsStageDir `
  --spec $SpecPath `
  --out-dir $OutDir `
  --version $Version `
  --build-id $BuildId `
  --signing-policy $SigningPolicy
if ($LASTEXITCODE -ne 0) {
  throw "aero_packager failed (exit $LASTEXITCODE)."
}

Write-Host "Done."

if ($CleanStage) {
  Write-Host "Cleaning staging directories..."
  Remove-Item -LiteralPath $driverPackRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $packagerDriversRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $guestToolsStageDir -Recurse -Force -ErrorAction SilentlyContinue
}

