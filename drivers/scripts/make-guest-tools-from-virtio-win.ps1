<#
.SYNOPSIS
Build Aero Guest Tools media (ISO + zip) from an upstream virtio-win ISO/root.

.DESCRIPTION
This script wraps:

- `drivers/scripts/make-driver-pack.ps1` (extracts Win7 driver packages from virtio-win)
- `tools/packaging/aero_packager/` (packages Guest Tools scripts + drivers into ISO/zip)

Use `-Profile` to choose a predictable driver set:

- `minimal` (default): storage+network only (aligned with the "minimal" packaging spec)
- `full`: includes optional audio/input drivers when present (best-effort)

Precedence:

- `-SpecPath` overrides the profile’s spec selection.
- `-Drivers` overrides the profile’s extraction driver list.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile full `
  -OutDir .\dist\guest-tools

.EXAMPLE
# Minimal packaging (storage+network only).
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-guest-tools-from-virtio-win.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso `
  -Profile minimal `
  -OutDir .\dist\guest-tools
#>

[CmdletBinding(DefaultParameterSetName = "FromIso")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutDir,

  [string]$Version = "0.0.0",

  [string]$BuildId = "local",

  # Driver signing / boot policy embedded in Guest Tools manifest.json.
  #
  # - test: media is intended for test-signed/custom-signed drivers
  # - production: media is intended for WHQL/production-signed drivers (default behavior: no cert injection, no Test Signing prompt)
  # - none: same as production (development use)
  #
  # Legacy aliases accepted:
  # - testsigning / test-signing -> test
  # - nointegritychecks / no-integrity-checks -> none
  # - prod / whql -> production
  [ValidateSet("none", "production", "test", "testsigning", "test-signing", "nointegritychecks", "no-integrity-checks", "prod", "whql")]
  [string]$SigningPolicy = "none",

  # Public certificate to embed under Guest Tools `certs/` when SigningPolicy=test.
  # For virtio-win this is typically unnecessary (WHQL/production-signed), but it is
  # required when packaging test-signed/custom-signed driver bundles.
  [string]$CertPath,

  # Packaging profile:
  # - minimal: storage+network only (default)
  # - full: includes optional virtio audio/input drivers if present
  [ValidateSet("minimal", "full")]
  [string]$Profile = "minimal",

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

$repoRoot = Resolve-RepoRoot

if (-not $OutDir) {
  $OutDir = Join-Path (Join-Path $repoRoot "dist") "guest-tools"
}

$specsDir = Join-Path (Join-Path (Join-Path $repoRoot "tools") "packaging") "specs"
$defaultSpecPath = if ($Profile -eq "minimal") {
  Join-Path $specsDir "win7-virtio-win.json"
} else {
  Join-Path $specsDir "win7-virtio-full.json"
}
$resolvedSpecPath = if ($PSBoundParameters.ContainsKey("SpecPath")) {
  if (-not $SpecPath) {
    throw "-SpecPath must not be empty."
  }
  if (-not [System.IO.Path]::IsPathRooted($SpecPath)) {
    Join-Path $repoRoot $SpecPath
  } else {
    $SpecPath
  }
} else {
  $defaultSpecPath
}
$resolvedSpecPath = [System.IO.Path]::GetFullPath($resolvedSpecPath)
$SpecPath = $resolvedSpecPath

$defaultDrivers = if ($Profile -eq "minimal") {
  @("viostor", "netkvm")
} else {
  @("viostor", "netkvm", "viosnd", "vioinput")
}
$resolvedDrivers = if ($PSBoundParameters.ContainsKey("Drivers")) {
  $Drivers
} else {
  $defaultDrivers
}
if ($null -eq $resolvedDrivers -or $resolvedDrivers.Count -eq 0) {
  throw "Resolved driver list is empty. Pass -Drivers <name>[,<name>...] or use -Profile minimal|full."
}

Write-Host "Resolved Guest Tools packaging inputs:"
Write-Host "  profile : $Profile"
Write-Host "  signing : $SigningPolicy"
Write-Host "  spec    : $resolvedSpecPath"
Write-Host "  drivers : $($resolvedDrivers -join ', ')"

$guestToolsDir = Join-Path $repoRoot "guest-tools"

$defaultCertPath = Join-Path (Join-Path $guestToolsDir "certs") "AeroTestRoot.cer"
$resolvedCertPath = if ($PSBoundParameters.ContainsKey("CertPath")) {
  if (-not $CertPath) {
    throw "-CertPath must not be empty."
  }
  if (-not [System.IO.Path]::IsPathRooted($CertPath)) {
    Join-Path $repoRoot $CertPath
  } else {
    $CertPath
  }
} else {
  $defaultCertPath
}
$resolvedCertPath = [System.IO.Path]::GetFullPath($resolvedCertPath)
$CertPath = $resolvedCertPath

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

# The Guest Tools packager now generates `config/devices.cmd` from a Windows device contract JSON.
# The in-repo contract uses Aero driver service names (aerovblk/aerovnet/...), but virtio-win uses
# upstream service names (viostor/netkvm/vioinput/viosnd). Create a temporary contract override so
# the packaged media seeds the correct service names for virtio-win.
$baseContractPath = Join-Path $repoRoot "docs/windows-device-contract.json"
if (-not (Test-Path -LiteralPath $baseContractPath -PathType Leaf)) {
  throw "Expected Windows device contract not found: $baseContractPath"
}
$virtioWinContractPath = Join-Path $driversOutDir "windows-device-contract.virtio-win.json"

try {
  $contractObj = Get-Content -LiteralPath $baseContractPath -Raw | ConvertFrom-Json
} catch {
  throw "Failed to parse device contract JSON: $baseContractPath`n$($_.Exception.Message)"
}
if ($null -eq $contractObj.devices) {
  throw "Device contract JSON is missing 'devices': $baseContractPath"
}

$updated = 0
foreach ($dev in $contractObj.devices) {
  $name = ("" + $dev.device).ToLowerInvariant()
  switch ($name) {
    "virtio-blk" { $dev.driver_service_name = "viostor"; $updated += 1 }
    "virtio-net" { $dev.driver_service_name = "netkvm"; $updated += 1 }
    "virtio-input" { $dev.driver_service_name = "vioinput"; $updated += 1 }
    "virtio-snd" { $dev.driver_service_name = "viosnd"; $updated += 1 }
  }
}
if ($updated -lt 2) {
  throw "Failed to patch virtio-win service names in device contract: $baseContractPath"
}

$contractJson = $contractObj | ConvertTo-Json -Depth 16
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($virtioWinContractPath, $contractJson, $utf8NoBom)
if (-not (Test-Path -LiteralPath $virtioWinContractPath -PathType Leaf)) {
  throw "Failed to write virtio-win device contract override: $virtioWinContractPath"
}
Write-Host "Using virtio-win device contract override:"
Write-Host "  base: $baseContractPath"
Write-Host "  out : $virtioWinContractPath"

# 1) Build the standard Win7 driver pack staging directory (from virtio-win ISO/root).
$packArgs = @(
  "-OutDir", $driversOutDir,
  "-NoZip"
)
$packArgs += "-Drivers"
$packArgs += $resolvedDrivers
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
if ($SigningPolicy -ne "test") {
  $certsDir = Join-Path $guestToolsStageDir "certs"
  if (Test-Path -LiteralPath $certsDir -PathType Container) {
    Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
      Remove-Item -Force -ErrorAction SilentlyContinue
  }
}

# 4) Build the actual Guest Tools ISO/zip via the standard CI wrapper script.
# Use the same staging logic as CI packaging so:
# - the staged Guest Tools config matches the packaged driver INF AddService name (viostor, etc)
# - outputs are deterministic via SOURCE_DATE_EPOCH / --source-date-epoch (wrapper default)
$wrapperScript = Join-Path (Join-Path $repoRoot "ci") "package-guest-tools.ps1"
if (-not (Test-Path -LiteralPath $wrapperScript -PathType Leaf)) {
  throw "Expected Guest Tools packaging wrapper script not found: $wrapperScript"
}

$deviceContractPath = Join-Path (Join-Path $repoRoot "docs") "windows-device-contract-virtio-win.json"
if (-not (Test-Path -LiteralPath $deviceContractPath -PathType Leaf)) {
  throw "Expected Windows device contract not found: $deviceContractPath"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools via CI wrapper..."
Write-Host "  spec : $SpecPath"
Write-Host "  out  : $OutDir"
Write-Host "  contract : $deviceContractPath"

& $wrapperScript `
  -InputRoot $packagerDriversRoot `
  -GuestToolsDir $guestToolsStageDir `
  -SigningPolicy $SigningPolicy `
  -CertPath $CertPath `
  -WindowsDeviceContractPath $virtioWinContractPath `
  -SpecPath $SpecPath `
  -WindowsDeviceContractPath $deviceContractPath `
  -OutDir $OutDir `
  -Version $Version `
  -BuildId $BuildId

Write-Host "Done."

if ($CleanStage) {
  Write-Host "Cleaning staging directories..."
  Remove-Item -LiteralPath $driverPackRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $packagerDriversRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $guestToolsStageDir -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $virtioWinContractPath -Force -ErrorAction SilentlyContinue
}

