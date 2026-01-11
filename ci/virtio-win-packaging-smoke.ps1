[CmdletBinding()]
param(
  [string]$OutRoot = (Join-Path $PSScriptRoot "..\out\virtio-win-packaging-smoke"),
  [switch]$OmitOptionalDrivers,
  [string]$GuestToolsSpecPath,
  # Controls the wrapper's extraction defaults (-Profile). When set to "auto" (default),
  # pick a profile that matches the well-known in-repo spec filenames.
  [ValidateSet("auto", "minimal", "full")]
  [string]$GuestToolsProfile = "auto"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function Assert-SafeOutRoot {
  param(
    [Parameter(Mandatory = $true)][string]$RepoRoot,
    [Parameter(Mandatory = $true)][string]$OutRoot
  )

  $repoFull = [System.IO.Path]::GetFullPath($RepoRoot)
  $outFull = [System.IO.Path]::GetFullPath($OutRoot)
  $driveRoot = [System.IO.Path]::GetPathRoot($outFull)

  if ($outFull -eq $repoFull) {
    throw "Refusing to use -OutRoot at the repo root (would delete the working tree): $outFull"
  }
  if ($outFull -eq $driveRoot) {
    throw "Refusing to use -OutRoot at the drive root: $outFull"
  }

  $repoOut = [System.IO.Path]::GetFullPath((Join-Path $repoFull "out"))
  if ($outFull.StartsWith($repoOut, [System.StringComparison]::OrdinalIgnoreCase)) {
    return
  }

  if ($outFull -match '(?i)virtio-win-packaging-smoke') {
    return
  }

  throw "Refusing to use -OutRoot outside '$repoOut' unless the path contains 'virtio-win-packaging-smoke': $outFull"
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
    [Parameter(Mandatory = $true)][string]$BaseName,
    [string]$HardwareId
  )

  $lines = New-Object "System.Collections.Generic.List[string]"
  $lines.Add("; Synthetic INF for CI virtio-win packaging smoke tests.") | Out-Null
  $lines.Add("[Version]") | Out-Null
  $lines.Add('Signature="$WINDOWS NT$"') | Out-Null
  $lines.Add("Class=System") | Out-Null
  $lines.Add("Provider=%ProviderName%") | Out-Null
  $lines.Add("") | Out-Null
  $lines.Add("[SourceDisksFiles]") | Out-Null
  $lines.Add("$BaseName.sys=1") | Out-Null
  $lines.Add("$BaseName.cat=1") | Out-Null

  if ($HardwareId) {
    $lines.Add("") | Out-Null
    $lines.Add("[HardwareIds]") | Out-Null
    $lines.Add($HardwareId) | Out-Null
  }

  $lines | Out-File -FilePath $Path -Encoding ascii
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
  Write-SyntheticInf -Path $infPath -BaseName $InfBaseName -HardwareId $HardwareId

  Write-PlaceholderBinary -Path (Join-Path $dir $sysName)
  Write-PlaceholderBinary -Path (Join-Path $dir $catName)
}

$repoRoot = Resolve-RepoRoot

if (-not $GuestToolsSpecPath) {
  $GuestToolsSpecPath = Join-Path $repoRoot "tools\packaging\specs\win7-virtio-win.json"
} elseif (-not [System.IO.Path]::IsPathRooted($GuestToolsSpecPath)) {
  $GuestToolsSpecPath = Join-Path $repoRoot $GuestToolsSpecPath
}
$GuestToolsSpecPath = [System.IO.Path]::GetFullPath($GuestToolsSpecPath)

if (-not (Test-Path -LiteralPath $GuestToolsSpecPath -PathType Leaf)) {
  throw "Guest Tools packaging spec not found: $GuestToolsSpecPath"
}

$resolvedGuestToolsProfile = $GuestToolsProfile
if ($resolvedGuestToolsProfile -eq "auto") {
  $specBaseName = [System.IO.Path]::GetFileName($GuestToolsSpecPath).ToLowerInvariant()
  if ($specBaseName -eq "win7-virtio-win.json") {
    $resolvedGuestToolsProfile = "minimal"
  } elseif ($specBaseName -eq "win7-virtio-full.json") {
    $resolvedGuestToolsProfile = "full"
  } else {
    # Best-effort fallback: default to full so the wrapper attempts to extract optional
    # virtio drivers unless explicitly constrained by -Drivers.
    $resolvedGuestToolsProfile = "full"
  }
}

if (-not [System.IO.Path]::IsPathRooted($OutRoot)) {
  $OutRoot = Join-Path $repoRoot $OutRoot
}
$OutRoot = [System.IO.Path]::GetFullPath($OutRoot)

Assert-SafeOutRoot -RepoRoot $repoRoot -OutRoot $OutRoot
Ensure-EmptyDirectory -Path $OutRoot

$logsDir = Join-Path $OutRoot "logs"
Ensure-Directory -Path $logsDir

$syntheticRoot = Join-Path $OutRoot "virtio-win"
Ensure-EmptyDirectory -Path $syntheticRoot

$osDir = "w7"

# Root-level license/notice files (best-effort copy). Use lowercase filenames to ensure
# packaging is robust on case-sensitive filesystems.
"license placeholder" | Out-File -FilePath (Join-Path $syntheticRoot "license.txt") -Encoding ascii
"notice placeholder" | Out-File -FilePath (Join-Path $syntheticRoot "notice.txt") -Encoding ascii
$fakeVirtioWinVersion = "0.0.0-synthetic"
$fakeVirtioWinVersion | Out-File -FilePath (Join-Path $syntheticRoot "VERSION") -Encoding ascii

# Root-mode provenance: simulate the JSON emitted by tools/virtio-win/extract.py so
# make-driver-pack.ps1 can record ISO hash/path even when -VirtioWinRoot is used.
$fakeIsoPath = "synthetic-virtio-win.iso"
$fakeIsoSha = ("0123456789abcdef" * 4)
$fakeIsoVolumeId = "SYNTH_VIRTIOWIN"
@{
  schema_version = 1
  virtio_win_iso = @{
    path = $fakeIsoPath
    sha256 = $fakeIsoSha
    volume_id = $fakeIsoVolumeId
  }
} | ConvertTo-Json -Depth 4 | Out-File -FilePath (Join-Path $syntheticRoot "virtio-win-provenance.json") -Encoding UTF8

New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1042"
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1042"

New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1041"
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1041"

if (-not $OmitOptionalDrivers) {
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1059"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1059"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1052"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1052"
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
  (Join-Path (Join-Path (Join-Path $driverPackRoot "licenses") "virtio-win") "license.txt"),
  (Join-Path (Join-Path (Join-Path $driverPackRoot "licenses") "virtio-win") "notice.txt"),
  (Join-Path $driverPackRoot "manifest.json")
 )) {
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "Expected driver pack output missing: $p"
  }
}

$driverPackManifestPath = Join-Path $driverPackRoot "manifest.json"
$driverPackManifest = Get-Content -LiteralPath $driverPackManifestPath -Raw | ConvertFrom-Json
if ($driverPackManifest.source.path -ne $fakeIsoPath) {
  throw "Driver pack manifest source.path mismatch: expected '$fakeIsoPath', got '$($driverPackManifest.source.path)'"
}
if ($driverPackManifest.source.derived_version -ne $fakeVirtioWinVersion) {
  throw "Driver pack manifest source.derived_version mismatch: expected '$fakeVirtioWinVersion', got '$($driverPackManifest.source.derived_version)'"
}
if (-not $driverPackManifest.source.hash -or $driverPackManifest.source.hash.value -ne $fakeIsoSha) {
  throw "Driver pack manifest source.hash mismatch: expected '$fakeIsoSha', got '$($driverPackManifest.source.hash.value)'"
}
if ($driverPackManifest.source.hash.algorithm -ne "sha256") {
  throw "Driver pack manifest source.hash.algorithm mismatch: expected 'sha256', got '$($driverPackManifest.source.hash.algorithm)'"
}
if ($driverPackManifest.source.volume_label -ne $fakeIsoVolumeId) {
  throw "Driver pack manifest source.volume_label mismatch: expected '$fakeIsoVolumeId', got '$($driverPackManifest.source.volume_label)'"
}
$noticeCopied = @($driverPackManifest.source.license_notice_files_copied)
foreach ($want in @("license.txt", "notice.txt")) {
  if (-not ($noticeCopied -contains $want)) {
    throw "Driver pack manifest did not record copied notice file '$want' in source.license_notice_files_copied. Got: $($noticeCopied -join ', ')"
  }
}
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

  # -StrictOptional is intended to catch missing optional drivers when they're explicitly requested.
  Write-Host "Validating -StrictOptional rejects missing optional drivers..."
  $strictPackOutDir = Join-Path $OutRoot "driver-pack-out-strict"
  Ensure-EmptyDirectory -Path $strictPackOutDir
  $strictPackLog = Join-Path $logsDir "make-driver-pack-strict.log"
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
    -VirtioWinRoot $syntheticRoot `
    -OutDir $strictPackOutDir `
    -Drivers "viostor","netkvm","viosnd","vioinput" `
    -StrictOptional `
    -NoZip *>&1 | Tee-Object -FilePath $strictPackLog
  if ($LASTEXITCODE -eq 0) {
    throw "Expected make-driver-pack.ps1 -StrictOptional to fail when optional drivers are missing. See $strictPackLog"
  }
} else {
  if ($driverPackManifest.optional_drivers_missing_any) {
    $missingNames = @($driverPackManifest.optional_drivers_missing | ForEach-Object { $_.name })
    throw "Expected make-driver-pack.ps1 to include optional drivers, but optional_drivers_missing_any=true. Missing: $($missingNames -join ', ')"
  }
  $driversIncluded = @($driverPackManifest.drivers | ForEach-Object { $_.ToString().ToLowerInvariant() })
  foreach ($want in @("viosnd", "vioinput")) {
    if (-not ($driversIncluded -contains $want)) {
      throw "Expected make-driver-pack.ps1 to include optional driver '$want' when present. Included drivers: $($driversIncluded -join ', ')"
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
  -Profile $resolvedGuestToolsProfile `
  -SpecPath $GuestToolsSpecPath `
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
foreach ($want in @(
  "THIRD_PARTY_NOTICES.md",
  "licenses/virtio-win/license.txt",
  "licenses/virtio-win/notice.txt",
  "licenses/virtio-win/driver-pack-manifest.json",
  "drivers/x86/viostor/viostor.inf",
  "drivers/x86/netkvm/netkvm.inf"
)) {
  if (-not ($manifestPaths -contains $want)) {
    throw "Guest Tools manifest missing expected packaged file path: $want"
  }
}

# When optional drivers are both present in the synthetic virtio-win tree and declared in the
# Guest Tools packaging spec, they should be included in the packaged output.
if (-not $OmitOptionalDrivers) {
  $specObj = Get-Content -LiteralPath $GuestToolsSpecPath -Raw | ConvertFrom-Json
  $specDriverNames = @()
  if ($null -ne $specObj.drivers) {
    $specDriverNames += @($specObj.drivers | ForEach-Object { $_.name })
  }
  if ($null -ne $specObj.required_drivers) {
    $specDriverNames += @($specObj.required_drivers | ForEach-Object { $_.name })
  }
  $specDriverNames = @($specDriverNames | Where-Object { $_ } | ForEach-Object { $_.ToString().ToLowerInvariant() } | Sort-Object -Unique)

  $optionalChecks = @()
  if ($specDriverNames -contains "viosnd") { $optionalChecks += "drivers/x86/viosnd/viosnd.inf"; $optionalChecks += "drivers/amd64/viosnd/viosnd.inf" }
  if ($specDriverNames -contains "vioinput") { $optionalChecks += "drivers/x86/vioinput/vioinput.inf"; $optionalChecks += "drivers/amd64/vioinput/vioinput.inf" }

  foreach ($want in $optionalChecks) {
    if (-not ($manifestPaths -contains $want)) {
      throw "Guest Tools manifest missing expected optional driver file path: $want"
    }
  }
}

$isoScript = Join-Path $repoRoot "drivers\scripts\make-virtio-driver-iso.ps1"
if (-not (Test-Path -LiteralPath $isoScript -PathType Leaf)) {
  throw "Expected script not found: $isoScript"
}

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

Write-Host "virtio-win packaging smoke test succeeded."
