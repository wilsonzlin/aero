[CmdletBinding()]
param(
  [string]$OutRoot = (Join-Path $PSScriptRoot "..\out\virtio-win-packaging-smoke"),
  [switch]$OmitOptionalDrivers
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
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

function Resolve-Python {
  $candidates = @("python3", "python", "py")
  foreach ($c in $candidates) {
    $cmd = Get-Command $c -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
  }
  return $null
}

function Write-SyntheticInf {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$HardwareId
  )
  @(
    "; Synthetic INF for CI virtio-win packaging smoke tests."
    "; Required hardware ID pattern:"
    $HardwareId
  ) | Out-File -FilePath $Path -Encoding ascii
}

function Write-PlaceholderBinary {
  param([Parameter(Mandatory = $true)][string]$Path)
  "placeholder" | Out-File -FilePath $Path -Encoding ascii
}

function New-SyntheticDriverFiles {
  param(
    [Parameter(Mandatory = $true)][string]$VirtioRoot,
    [Parameter(Mandatory = $true)][string]$UpstreamDirName,
    [Parameter(Mandatory = $true)][string]$InfBaseName,
    [Parameter(Mandatory = $true)][string]$OsDirName,
    [Parameter(Mandatory = $true)][string]$ArchDirName,
    [string]$HardwareId
  )

  $dir = Join-Path $VirtioRoot (Join-Path $UpstreamDirName (Join-Path $OsDirName $ArchDirName))
  Ensure-Directory -Path $dir

  $infName = "$InfBaseName.inf"
  $sysName = "$InfBaseName.sys"
  $catName = "$InfBaseName.cat"

  $infPath = Join-Path $dir $infName
  if ($HardwareId) {
    Write-SyntheticInf -Path $infPath -HardwareId $HardwareId
  } else {
    @(
      "; Synthetic INF for CI virtio-win packaging smoke tests."
      "; No HWID validation required for this driver in tools/packaging/specs/win7-virtio-win.json"
    ) | Out-File -FilePath $infPath -Encoding ascii
  }

  Write-PlaceholderBinary -Path (Join-Path $dir $sysName)
  Write-PlaceholderBinary -Path (Join-Path $dir $catName)
}

$repoRoot = Resolve-RepoRoot

if (-not [System.IO.Path]::IsPathRooted($OutRoot)) {
  $OutRoot = Join-Path $repoRoot $OutRoot
}
$OutRoot = [System.IO.Path]::GetFullPath($OutRoot)

Ensure-EmptyDirectory -Path $OutRoot

$logsDir = Join-Path $OutRoot "logs"
Ensure-Directory -Path $logsDir

$syntheticRoot = Join-Path $OutRoot "virtio-win"
Ensure-EmptyDirectory -Path $syntheticRoot

$osDir = "w7"

New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1042"
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1042"

New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1041"
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1041"

if (-not $OmitOptionalDrivers) {
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "x86"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "amd64"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "x86"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "amd64"
}

$driverPackOutDir = Join-Path $OutRoot "driver-pack-out"
Ensure-EmptyDirectory -Path $driverPackOutDir

$driverPackScript = Join-Path $repoRoot "drivers\scripts\make-driver-pack.ps1"
$driverPackLog = Join-Path $logsDir "make-driver-pack.log"

Write-Host "Running make-driver-pack.ps1..."
& pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
  -VirtioWinRoot $syntheticRoot `
  -OutDir $driverPackOutDir `
  -NoZip *>&1 | Tee-Object -FilePath $driverPackLog
if ($LASTEXITCODE -ne 0) {
  throw "make-driver-pack.ps1 failed (exit $LASTEXITCODE). See $driverPackLog"
}

$driverPackRoot = Join-Path $driverPackOutDir "aero-win7-driver-pack"
if (-not (Test-Path -LiteralPath $driverPackRoot -PathType Container)) {
  throw "Expected driver pack staging directory not found: $driverPackRoot"
}

foreach ($p in @(
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.inf"),
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.sys"),
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.cat"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.inf"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.sys"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.cat"),
  (Join-Path $driverPackRoot "win7\amd64\viostor\viostor.inf"),
  (Join-Path $driverPackRoot "win7\amd64\netkvm\netkvm.inf"),
  (Join-Path $driverPackRoot "install.cmd"),
  (Join-Path $driverPackRoot "enable-testsigning.cmd"),
  (Join-Path $driverPackRoot "THIRD_PARTY_NOTICES.md"),
  (Join-Path $driverPackRoot "README.md"),
  (Join-Path $driverPackRoot "manifest.json")
)) {
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "Expected driver pack output missing: $p"
  }
}

$driverPackManifestPath = Join-Path $driverPackRoot "manifest.json"
$driverPackManifest = Get-Content -LiteralPath $driverPackManifestPath -Raw | ConvertFrom-Json
if ($OmitOptionalDrivers) {
  if (-not $driverPackManifest.optional_drivers_missing_any) {
    throw "Expected make-driver-pack.ps1 to report optional drivers missing, but optional_drivers_missing_any=false."
  }
  $missingNames = @($driverPackManifest.optional_drivers_missing | ForEach-Object { $_.name })
  foreach ($want in @("viosnd", "vioinput")) {
    if (-not ($missingNames -contains $want)) {
      throw "Expected make-driver-pack.ps1 to report missing optional driver '$want'. Reported: $($missingNames -join ', ')"
    }
  }
}

$guestToolsOutDir = Join-Path $OutRoot "guest-tools-out"
Ensure-EmptyDirectory -Path $guestToolsOutDir

$guestToolsScript = Join-Path $repoRoot "drivers\scripts\make-guest-tools-from-virtio-win.ps1"
$guestToolsLog = Join-Path $logsDir "make-guest-tools-from-virtio-win.log"

Write-Host "Running make-guest-tools-from-virtio-win.ps1..."
& pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript `
  -VirtioWinRoot $syntheticRoot `
  -OutDir $guestToolsOutDir `
  -Version "0.0.0" `
  -BuildId "ci" `
  -CleanStage *>&1 | Tee-Object -FilePath $guestToolsLog
if ($LASTEXITCODE -ne 0) {
  throw "make-guest-tools-from-virtio-win.ps1 failed (exit $LASTEXITCODE). See $guestToolsLog"
}

$guestIso = Join-Path $guestToolsOutDir "aero-guest-tools.iso"
$guestZip = Join-Path $guestToolsOutDir "aero-guest-tools.zip"
$guestManifest = Join-Path $guestToolsOutDir "manifest.json"

foreach ($p in @($guestIso, $guestZip, $guestManifest)) {
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "Expected Guest Tools output missing: $p"
  }
}

$manifestObj = Get-Content -LiteralPath $guestManifest -Raw | ConvertFrom-Json
if ($manifestObj.package.version -ne "0.0.0") {
  throw "Guest Tools manifest version mismatch: expected 0.0.0, got $($manifestObj.package.version)"
}
if ($manifestObj.package.build_id -ne "ci") {
  throw "Guest Tools manifest build_id mismatch: expected ci, got $($manifestObj.package.build_id)"
}

$manifestPaths = @($manifestObj.files | ForEach-Object { $_.path })
foreach ($want in @("drivers/x86/viostor/viostor.inf", "drivers/x86/netkvm/netkvm.inf")) {
  if (-not ($manifestPaths -contains $want)) {
    throw "Guest Tools manifest missing expected packaged file path: $want"
  }
}

$isoScript = Join-Path $repoRoot "drivers\scripts\make-virtio-driver-iso.ps1"
$xorriso = Get-Command "xorriso" -ErrorAction SilentlyContinue
if ($xorriso) {
  $python = Resolve-Python
  if (-not $python) {
    throw "Python not found on PATH; required for virtio driver ISO validation."
  }

  $driverIsoPath = Join-Path $OutRoot "aero-virtio-win7-drivers.iso"
  $driverIsoLog = Join-Path $logsDir "make-virtio-driver-iso.log"
  $verifyIsoLog = Join-Path $logsDir "verify-virtio-driver-iso.log"

  Write-Host "Running make-virtio-driver-iso.ps1..."
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $isoScript `
    -VirtioWinRoot $syntheticRoot `
    -OutIso $driverIsoPath *>&1 | Tee-Object -FilePath $driverIsoLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-virtio-driver-iso.ps1 failed (exit $LASTEXITCODE). See $driverIsoLog"
  }

  if (-not (Test-Path -LiteralPath $driverIsoPath -PathType Leaf)) {
    throw "Expected driver ISO output missing: $driverIsoPath"
  }

  Write-Host "Verifying driver ISO contents..."
  & $python (Join-Path $repoRoot "tools\driver-iso\verify_iso.py") `
    --iso $driverIsoPath *>&1 | Tee-Object -FilePath $verifyIsoLog
  if ($LASTEXITCODE -ne 0) {
    throw "verify_iso.py failed (exit $LASTEXITCODE). See $verifyIsoLog"
  }
} else {
  Write-Host "xorriso not found; skipping make-virtio-driver-iso.ps1 smoke test."
}

Write-Host "virtio-win packaging smoke test succeeded."
