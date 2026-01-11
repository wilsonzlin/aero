#Requires -Version 5.1
#
# CI-friendly wrapper around the deterministic Rust Guest Tools packager.
# Produces:
#   - out/artifacts/aero-guest-tools.iso
#   - out/artifacts/aero-guest-tools.zip
#   - out/artifacts/aero-guest-tools.manifest.json
#
# Inputs are signed driver packages produced by the existing Win7 driver pipeline
# (typically `out/packages/`) OR an extracted `*-bundle.zip` produced by
# `ci/package-drivers.ps1`.

[CmdletBinding()]
param(
  # Root containing signed driver packages. Common inputs:
  #   - out/packages/                         (CI staging output)
  #   - <extracted AeroVirtIO-Win7-*-bundle>/ (output of ci/package-drivers.ps1)
  [string] $InputRoot = "out/packages",

  # Source Guest Tools directory (scripts/config/certs).
  [string] $GuestToolsDir = "guest-tools",

  # Driver signing / boot policy embedded in Guest Tools manifest.json.
  #
  # - testsigning: media is intended for test-signed/custom-signed drivers (default)
  # - nointegritychecks: media may prompt to disable signature enforcement (not recommended)
  # - none: media is intended for WHQL/production-signed drivers (no cert injection)
  [ValidateSet("none", "testsigning", "nointegritychecks")]
  [string] $SigningPolicy = "testsigning",

  # Public certificate used to sign the driver catalogs (required unless SigningPolicy=none).
  [string] $CertPath = "out/certs/aero-test.cer",

  # Output directory for final artifacts.
  [string] $OutDir = "out/artifacts",

  # Version string embedded in manifest.json (defaults to the same derived format as ci/package-drivers.ps1).
  [string] $Version,

  # Build identifier embedded in manifest.json (defaults to the git short SHA).
  [string] $BuildId,

  # Optional override for SOURCE_DATE_EPOCH. If not provided:
  #   - uses $env:SOURCE_DATE_EPOCH when set
  #   - otherwise uses the HEAD commit timestamp (git) for determinism
  [Nullable[long]] $SourceDateEpoch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoPath {
  param([Parameter(Mandatory = $true)][string] $Path)

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  $repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string] $Path)
  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Get-VersionString {
  # Kept intentionally in sync with ci/package-drivers.ps1 so driver bundle
  # versions and Guest Tools versions line up by default.
  function Parse-SemverTag {
    param([Parameter(Mandatory = $true)][string] $Tag)

    $clean = $Tag.Trim()
    if ($clean.StartsWith("v")) {
      $clean = $clean.Substring(1)
    }

    if ($clean -match "^([0-9]+)\\.([0-9]+)\\.([0-9]+)$") {
      return [pscustomobject]@{
        Major = [int] $Matches[1]
        Minor = [int] $Matches[2]
        Patch = [int] $Matches[3]
        Text  = $clean
      }
    }

    return $null
  }

  $repoRoot = $null
  $sha = $null
  $commitDate = $null
  $tag = $null

  try {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
    $sha = (& git -C $repoRoot rev-parse --short=12 HEAD 2>$null).Trim()
    $commitDateIso = (& git -C $repoRoot show -s --format=%cI HEAD 2>$null).Trim()
    if (-not [string]::IsNullOrWhiteSpace($commitDateIso)) {
      $commitDate = [DateTimeOffset]::Parse($commitDateIso, [System.Globalization.CultureInfo]::InvariantCulture)
    }
    $tag = (& git -C $repoRoot describe --tags --abbrev=0 --match "v[0-9]*" 2>$null).Trim()
    if ([string]::IsNullOrWhiteSpace($tag)) {
      $tag = $null
    }
  } catch {
    $sha = $null
    $commitDate = $null
    $tag = $null
  }

  $date = $null
  if ($null -ne $commitDate) {
    $date = $commitDate.ToString("yyyyMMdd", [System.Globalization.CultureInfo]::InvariantCulture)
  } else {
    $date = Get-Date -Format "yyyyMMdd"
  }

  if ([string]::IsNullOrWhiteSpace($sha) -or [string]::IsNullOrWhiteSpace($repoRoot)) {
    return $date
  }

  $base = $null
  if ($tag) {
    $base = Parse-SemverTag $tag
  }
  if (-not $base) {
    $base = [pscustomobject]@{ Major = 0; Minor = 0; Patch = 0; Text = "0.0.0" }
  }

  $distance = 0
  if ($tag) {
    try {
      $distance = [int] ((& git -C $repoRoot rev-list --count "$tag..HEAD" 2>$null) | Out-String).Trim()
    } catch {
      $distance = [int] ((& git -C $repoRoot rev-list --count HEAD 2>$null) | Out-String).Trim()
    }
  } else {
    $distance = [int] ((& git -C $repoRoot rev-list --count HEAD 2>$null) | Out-String).Trim()
  }

  $semver = $null
  if ($distance -eq 0) {
    $semver = "{0}+g{1}" -f $base.Text, $sha
  } else {
    $semver = "{0}+{1}.g{2}" -f $base.Text, $distance, $sha
  }

  return "$date-$semver"
}

function Try-GetGitValue {
  param([Parameter(Mandatory = $true)][string[]] $Args)
  try {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
    $out = (& git -C $repoRoot @Args 2>$null)
    if ($LASTEXITCODE -ne 0) {
      return $null
    }
    return ([string]($out | Out-String)).Trim()
  } catch {
    return $null
  }
}

function Get-DefaultBuildId {
  $sha = Try-GetGitValue -Args @("rev-parse", "--short=12", "HEAD")
  if (-not [string]::IsNullOrWhiteSpace($sha)) {
    return $sha
  }
  return "local"
}

function Get-DefaultSourceDateEpoch {
  # Honor explicit env var first.
  if (-not [string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
    try {
      return [long] $env:SOURCE_DATE_EPOCH
    } catch {
      # fall through
    }
  }

  # Deterministic per commit.
  $ct = Try-GetGitValue -Args @("show", "-s", "--format=%ct", "HEAD")
  if (-not [string]::IsNullOrWhiteSpace($ct)) {
    try {
      return [long] $ct
    } catch {
      # fall through
    }
  }

  return 0
}

function Assert-ContainsFileExtension {
  param(
    [Parameter(Mandatory = $true)][string] $Root,
    [Parameter(Mandatory = $true)][string] $Extension
  )

  $pattern = "*.$Extension"
  $found = Get-ChildItem -LiteralPath $Root -Recurse -File -Filter $pattern -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $found) {
    throw "Expected at least one '$pattern' file under '$Root'."
  }
}

function Copy-GuestToolsWithCert {
  param(
    [Parameter(Mandatory = $true)][string] $SourceDir,
    [Parameter(Mandatory = $true)][string] $DestDir,
    [Parameter(Mandatory = $true)][string] $CertSourcePath
  )

  Copy-Item -LiteralPath $SourceDir -Destination $DestDir -Recurse -Force

  $certsDir = Join-Path $DestDir "certs"
  if (-not (Test-Path -LiteralPath $certsDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $certsDir | Out-Null
  }

  # Replace any placeholder certs in the staged Guest Tools tree so the output ISO
  # matches the signing certificate used for the packaged driver catalogs.
  Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
    Remove-Item -Force -ErrorAction SilentlyContinue

  Copy-Item -LiteralPath $CertSourcePath -Destination (Join-Path $certsDir "aero-test.cer") -Force
}

function Copy-GuestToolsWithoutCert {
  param(
    [Parameter(Mandatory = $true)][string] $SourceDir,
    [Parameter(Mandatory = $true)][string] $DestDir
  )

  Copy-Item -LiteralPath $SourceDir -Destination $DestDir -Recurse -Force

  $certsDir = Join-Path $DestDir "certs"
  if (Test-Path -LiteralPath $certsDir -PathType Container) {
    Get-ChildItem -LiteralPath $certsDir -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Extension -in @(".cer", ".crt", ".p7b") } |
      Remove-Item -Force -ErrorAction SilentlyContinue
  }
}

function Resolve-ArchDirName {
  param(
    [Parameter(Mandatory = $true)][string] $DriversRoot,
    [Parameter(Mandatory = $true)][ValidateSet("x86", "amd64")][string] $ArchOut
  )

  $candidates = @()
  if ($ArchOut -eq "x86") {
    $candidates = @("x86", "win32", "i386")
  } else {
    $candidates = @("amd64", "x64", "x86_64", "x86-64")
  }

  foreach ($name in $candidates) {
    $p = Join-Path $DriversRoot $name
    if (Test-Path -LiteralPath $p -PathType Container) {
      return $p
    }
  }

  return $null
}

function Stage-DriversFromPackagerLayout {
  param(
    [Parameter(Mandatory = $true)][string] $InputDriversRoot,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  $x86Src = Resolve-ArchDirName -DriversRoot $InputDriversRoot -ArchOut "x86"
  $amd64Src = Resolve-ArchDirName -DriversRoot $InputDriversRoot -ArchOut "amd64"

  if (-not $x86Src -or -not $amd64Src) {
    throw "Input drivers root does not look like packager layout (expected x86/ and amd64/ or x64/): '$InputDriversRoot'."
  }

  foreach ($arch in @([pscustomobject]@{ Src = $x86Src; Out = "x86" }, [pscustomobject]@{ Src = $amd64Src; Out = "amd64" })) {
    $destArchDir = Join-Path $StageDriversRoot $arch.Out
    New-Item -ItemType Directory -Force -Path $destArchDir | Out-Null

    $driverDirs = Get-ChildItem -LiteralPath $arch.Src -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name
    foreach ($d in $driverDirs) {
      $dst = Join-Path $destArchDir $d.Name
      Copy-Item -LiteralPath $d.FullName -Destination $dst -Recurse -Force
    }
  }
}

function Stage-DriversFromBundleLayout {
  param(
    [Parameter(Mandatory = $true)][string] $BundleRoot,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  $driversRoot = Join-Path $BundleRoot "drivers"
  if (-not (Test-Path -LiteralPath $driversRoot -PathType Container)) {
    throw "Bundle root missing expected 'drivers' directory: '$driversRoot'."
  }

  $destX86 = Join-Path $StageDriversRoot "x86"
  $destAmd64 = Join-Path $StageDriversRoot "amd64"
  New-Item -ItemType Directory -Force -Path $destX86 | Out-Null
  New-Item -ItemType Directory -Force -Path $destAmd64 | Out-Null

  $driverDirs = Get-ChildItem -LiteralPath $driversRoot -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name
  foreach ($d in $driverDirs) {
    $srcX86 = Join-Path $d.FullName "x86"
    if (Test-Path -LiteralPath $srcX86 -PathType Container) {
      $dst = Join-Path $destX86 $d.Name
      New-Item -ItemType Directory -Force -Path $dst | Out-Null
      Copy-Item -Path (Join-Path $srcX86 "*") -Destination $dst -Recurse -Force
    }

    $srcX64 = $null
    foreach ($cand in @("amd64", "x64")) {
      $p = Join-Path $d.FullName $cand
      if (Test-Path -LiteralPath $p -PathType Container) {
        $srcX64 = $p
        break
      }
    }
    if ($srcX64) {
      $dst = Join-Path $destAmd64 $d.Name
      New-Item -ItemType Directory -Force -Path $dst | Out-Null
      Copy-Item -Path (Join-Path $srcX64 "*") -Destination $dst -Recurse -Force
    }
  }
}

function Stage-DriversFromPackagesLayout {
  param(
    [Parameter(Mandatory = $true)][string] $PackagesRoot,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  $archDirs = Get-ChildItem -LiteralPath $PackagesRoot -Recurse -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -in @("x86", "x64", "amd64", "win32", "i386") }

  if (-not $archDirs) {
    throw "No architecture directories (x86/x64/amd64) found under '$PackagesRoot'."
  }

  $records = New-Object System.Collections.Generic.List[object]
  foreach ($archDir in $archDirs) {
    $inf = Get-ChildItem -LiteralPath $archDir.FullName -File -Filter "*.inf" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $inf) {
      continue
    }

    $archOut = $null
    switch ($archDir.Name.ToLowerInvariant()) {
      "x86" { $archOut = "x86" }
      "win32" { $archOut = "x86" }
      "i386" { $archOut = "x86" }
      "x64" { $archOut = "amd64" }
      "amd64" { $archOut = "amd64" }
      default { $archOut = $null }
    }
    if (-not $archOut) { continue }

    $driverRoot = Split-Path -Parent $archDir.FullName
    $driverNameCandidate = Split-Path -Leaf $driverRoot

    $rel = $driverRoot
    $inputTrimmed = $PackagesRoot.TrimEnd("\", "/")
    if ($driverRoot.Length -ge $inputTrimmed.Length -and $driverRoot.StartsWith($inputTrimmed, [System.StringComparison]::OrdinalIgnoreCase)) {
      $rel = $driverRoot.Substring($inputTrimmed.Length).TrimStart("\", "/")
    }

    [void]$records.Add([pscustomobject]@{
      ArchOut = $archOut
      SrcDir = $archDir.FullName
      DriverNameCandidate = $driverNameCandidate
      DriverRootRel = $rel
    })
  }

  if ($records.Count -eq 0) {
    throw "No driver package directories containing .inf files were found under '$PackagesRoot'."
  }

  # Avoid collisions when multiple driver roots share the same leaf directory name.
  $byCandidate = @{}
  foreach ($r in $records) {
    $key = $r.DriverNameCandidate.ToLowerInvariant()
    if (-not $byCandidate.ContainsKey($key)) {
      $byCandidate[$key] = New-Object System.Collections.Generic.List[object]
    }
    [void]$byCandidate[$key].Add($r)
  }

  foreach ($r in $records) {
    $candidateKey = $r.DriverNameCandidate.ToLowerInvariant()
    $list = $byCandidate[$candidateKey]
    $finalName = $r.DriverNameCandidate
    if ($list.Count -gt 1) {
      $finalName = $r.DriverRootRel.Replace("\", "-").Replace("/", "-")
    }
    $r | Add-Member -NotePropertyName DriverName -NotePropertyValue $finalName -Force
  }

  $destX86 = Join-Path $StageDriversRoot "x86"
  $destAmd64 = Join-Path $StageDriversRoot "amd64"
  New-Item -ItemType Directory -Force -Path $destX86 | Out-Null
  New-Item -ItemType Directory -Force -Path $destAmd64 | Out-Null

  # Group sources by the destination key to avoid merging unrelated directories.
  $byDest = @{}
  foreach ($r in $records) {
    $key = "{0}|{1}" -f $r.ArchOut, $r.DriverName.ToLowerInvariant()
    if (-not $byDest.ContainsKey($key)) {
      $byDest[$key] = New-Object System.Collections.Generic.List[object]
    }
    [void]$byDest[$key].Add($r)
  }

  foreach ($key in ($byDest.Keys | Sort-Object)) {
    $items = $byDest[$key]
    if ($items.Count -gt 1) {
      $paths = ($items | Select-Object -ExpandProperty SrcDir | Sort-Object -Unique) -join ", "
      throw "Multiple source directories map to the same staged driver directory ($key): $paths"
    }

    $r = $items[0]
    $destArchDir = if ($r.ArchOut -eq "x86") { $destX86 } else { $destAmd64 }
    $destDir = Join-Path $destArchDir $r.DriverName
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item -Path (Join-Path $r.SrcDir "*") -Destination $destDir -Recurse -Force
  }
}

function New-PackagerSpecFromStagedDrivers {
  param(
    [Parameter(Mandatory = $true)][string] $StageDriversRoot,
    [Parameter(Mandatory = $true)][string] $SpecOutPath
  )

  $x86Dir = Join-Path $StageDriversRoot "x86"
  $amd64Dir = Join-Path $StageDriversRoot "amd64"
  if (-not (Test-Path -LiteralPath $x86Dir -PathType Container)) {
    throw "Missing staged drivers directory: $x86Dir"
  }
  if (-not (Test-Path -LiteralPath $amd64Dir -PathType Container)) {
    throw "Missing staged drivers directory: $amd64Dir"
  }

  $x86Drivers = @(Get-ChildItem -LiteralPath $x86Dir -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name)
  $amd64Drivers = @(Get-ChildItem -LiteralPath $amd64Dir -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name)

  if (-not $x86Drivers -or $x86Drivers.Count -eq 0) {
    throw "No staged x86 driver directories found under: $x86Dir"
  }
  if (-not $amd64Drivers -or $amd64Drivers.Count -eq 0) {
    throw "No staged amd64 driver directories found under: $amd64Dir"
  }

  # Basic sanity: ensure each staged driver directory contains the expected file types.
  foreach ($d in $x86Drivers) {
    Assert-ContainsFileExtension -Root $d.FullName -Extension "inf"
    Assert-ContainsFileExtension -Root $d.FullName -Extension "sys"
    Assert-ContainsFileExtension -Root $d.FullName -Extension "cat"
  }
  foreach ($d in $amd64Drivers) {
    Assert-ContainsFileExtension -Root $d.FullName -Extension "inf"
    Assert-ContainsFileExtension -Root $d.FullName -Extension "sys"
    Assert-ContainsFileExtension -Root $d.FullName -Extension "cat"
  }

  $setX86 = @{}
  foreach ($d in $x86Drivers) { $setX86[$d.Name.ToLowerInvariant()] = $d.Name }
  $setAmd64 = @{}
  foreach ($d in $amd64Drivers) { $setAmd64[$d.Name.ToLowerInvariant()] = $d.Name }

  $common = New-Object System.Collections.Generic.List[string]
  foreach ($k in $setX86.Keys) {
    if ($setAmd64.ContainsKey($k)) {
      [void]$common.Add($setX86[$k])
    }
  }

  if ($common.Count -eq 0) {
    $x86Names = ($x86Drivers | Select-Object -ExpandProperty Name | Sort-Object) -join ", "
    $amd64Names = ($amd64Drivers | Select-Object -ExpandProperty Name | Sort-Object) -join ", "
    throw "No driver directories were staged for BOTH x86 and amd64; refusing to package Guest Tools. x86=[$x86Names] amd64=[$amd64Names]"
  }

  # Emit a packager spec that lists:
  # - drivers present for both x86+amd64 as required=true
  # - drivers present for only one architecture as required=false (best-effort; packager will warn + skip missing arch)
  $allKeys = @{}
  foreach ($k in $setX86.Keys) { $allKeys[$k] = $true }
  foreach ($k in $setAmd64.Keys) { $allKeys[$k] = $true }

  $drivers = @()
  foreach ($k in ($allKeys.Keys | Sort-Object)) {
    $name = $null
    if ($setX86.ContainsKey($k)) {
      $name = $setX86[$k]
    } else {
      $name = $setAmd64[$k]
    }
    $isRequired = $setX86.ContainsKey($k) -and $setAmd64.ContainsKey($k)

    $drivers += @{
      name = $name
      required = $isRequired
      expected_hardware_ids = @()
    }
  }

  $spec = @{
    drivers = $drivers
  }

  $json = $spec | ConvertTo-Json -Depth 10
  # serde_json does NOT accept UTF-8 BOM, and Windows PowerShell 5.1 writes BOM by
  # default for `-Encoding UTF8`. Always write UTF-8 without BOM so the Rust packager
  # can parse the generated spec reliably on all hosts.
  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllText($SpecOutPath, $json, $utf8NoBom)
}

$repoRootResolved = Resolve-RepoPath -Path "."
$inputRootResolved = Resolve-RepoPath -Path $InputRoot
$guestToolsResolved = Resolve-RepoPath -Path $GuestToolsDir
$certPathResolved = Resolve-RepoPath -Path $CertPath
$outDirResolved = Resolve-RepoPath -Path $OutDir

if (-not (Test-Path -LiteralPath $inputRootResolved)) {
  throw "InputRoot does not exist: '$inputRootResolved'."
}
if (-not (Test-Path -LiteralPath $guestToolsResolved -PathType Container)) {
  throw "GuestToolsDir does not exist: '$guestToolsResolved'."
}
if ($SigningPolicy -ine "none") {
  if (-not (Test-Path -LiteralPath $certPathResolved -PathType Leaf)) {
    throw "CertPath does not exist: '$certPathResolved'."
  }
}

if ([string]::IsNullOrWhiteSpace($Version)) {
  $Version = Get-VersionString
}
if ([string]::IsNullOrWhiteSpace($BuildId)) {
  $BuildId = Get-DefaultBuildId
}

$epoch = $null
if ($PSBoundParameters.ContainsKey("SourceDateEpoch") -and $null -ne $SourceDateEpoch) {
  $epoch = [long] $SourceDateEpoch
} else {
  $epoch = Get-DefaultSourceDateEpoch
}
$env:SOURCE_DATE_EPOCH = [string]$epoch

New-Item -ItemType Directory -Force -Path $outDirResolved | Out-Null

$stageRoot = Resolve-RepoPath -Path "out/_staging_guest_tools"
$stageDriversRoot = Join-Path $stageRoot "drivers"
$stageGuestTools = Join-Path $stageRoot "guest-tools"
$stageInputExtract = Join-Path $stageRoot "input"
$stageSpec = Join-Path $stageRoot "spec.json"
$stagePackagerOut = Join-Path $stageRoot "packager_out"

$success = $false
try {
  Ensure-EmptyDirectory -Path $stageRoot
  Ensure-EmptyDirectory -Path $stageDriversRoot
  Ensure-EmptyDirectory -Path $stageInputExtract
  Ensure-EmptyDirectory -Path $stagePackagerOut

  Write-Host "Staging Guest Tools..."
  if ($SigningPolicy -ine "none") {
    Copy-GuestToolsWithCert -SourceDir $guestToolsResolved -DestDir $stageGuestTools -CertSourcePath $certPathResolved
  } else {
    Copy-GuestToolsWithoutCert -SourceDir $guestToolsResolved -DestDir $stageGuestTools
  }

  Write-Host "Staging signed drivers..."

  $inputRootForStaging = $inputRootResolved
  $inputExt = [System.IO.Path]::GetExtension($inputRootResolved).ToLowerInvariant()
  if ((Test-Path -LiteralPath $inputRootResolved -PathType Leaf) -and ($inputExt -eq ".zip")) {
    Write-Host "  Extracting driver bundle ZIP: $inputRootResolved"
    Expand-Archive -LiteralPath $inputRootResolved -DestinationPath $stageInputExtract -Force

    $topDirs = @(Get-ChildItem -LiteralPath $stageInputExtract -Directory -ErrorAction SilentlyContinue)
    $topFiles = @(Get-ChildItem -LiteralPath $stageInputExtract -File -ErrorAction SilentlyContinue)
    if ($topDirs.Count -eq 1 -and $topFiles.Count -eq 0) {
      $inputRootForStaging = $topDirs[0].FullName
    } else {
      $inputRootForStaging = $stageInputExtract
    }
  }

  $bundleDriversDir = Join-Path $inputRootForStaging "drivers"
  $looksLikeBundle = (Test-Path -LiteralPath $bundleDriversDir -PathType Container) -and (Get-ChildItem -LiteralPath $bundleDriversDir -Directory -ErrorAction SilentlyContinue | Select-Object -First 1)

  $looksLikePackagerLayout = $false
  $x86Maybe = Resolve-ArchDirName -DriversRoot $inputRootForStaging -ArchOut "x86"
  $amd64Maybe = Resolve-ArchDirName -DriversRoot $inputRootForStaging -ArchOut "amd64"
  if ($x86Maybe -and $amd64Maybe) {
    $hasDriverDirs = (Get-ChildItem -LiteralPath $x86Maybe -Directory -ErrorAction SilentlyContinue | Select-Object -First 1) -and (Get-ChildItem -LiteralPath $amd64Maybe -Directory -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($hasDriverDirs) {
      $looksLikePackagerLayout = $true
    }
  }

  if ($looksLikePackagerLayout) {
    Write-Host "  Detected input layout: packager (x86/ + amd64/)"
    Stage-DriversFromPackagerLayout -InputDriversRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  } elseif ($looksLikeBundle) {
    Write-Host "  Detected input layout: driver bundle (drivers/<name>/(x86|x64)/...)"
    Stage-DriversFromBundleLayout -BundleRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  } else {
    Write-Host "  Detected input layout: CI packages (out/packages/<driver>/<arch>/...)"
    Stage-DriversFromPackagesLayout -PackagesRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  }

  Write-Host "Generating packager spec..."
  New-PackagerSpecFromStagedDrivers -StageDriversRoot $stageDriversRoot -SpecOutPath $stageSpec

  $packagerManifest = Resolve-RepoPath -Path "tools/packaging/aero_packager/Cargo.toml"
  if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
    throw "Missing packager Cargo.toml: '$packagerManifest'."
  }

  $volumeId = ("AERO_GUEST_TOOLS_" + $Version).ToUpperInvariant() -replace "[^A-Z0-9_]", "_"
  if ($volumeId.Length -gt 32) {
    $volumeId = $volumeId.Substring(0, 32)
  }

  Write-Host "Packaging via aero_packager..."
  Write-Host "  version: $Version"
  Write-Host "  build  : $BuildId"
  Write-Host "  epoch  : $epoch"
  Write-Host "  policy : $SigningPolicy"
  Write-Host "  out    : $outDirResolved"

  & cargo run --manifest-path $packagerManifest --release --locked -- `
    --drivers-dir $stageDriversRoot `
    --guest-tools-dir $stageGuestTools `
    --spec $stageSpec `
    --out-dir $stagePackagerOut `
    --version $Version `
    --build-id $BuildId `
    --volume-id $volumeId `
    --signing-policy $SigningPolicy `
    --source-date-epoch $epoch
  if ($LASTEXITCODE -ne 0) {
    throw "aero_packager failed (exit code $LASTEXITCODE)."
  }

  $isoSrc = Join-Path $stagePackagerOut "aero-guest-tools.iso"
  $zipSrc = Join-Path $stagePackagerOut "aero-guest-tools.zip"
  $manifestSrc = Join-Path $stagePackagerOut "manifest.json"

  foreach ($p in @($isoSrc, $zipSrc, $manifestSrc)) {
    $item = Get-Item -LiteralPath $p -ErrorAction SilentlyContinue
    if (-not $item -or $item.Length -le 0) {
      throw "Expected packager output file missing or empty: $p"
    }
  }

  $isoDest = Join-Path $outDirResolved "aero-guest-tools.iso"
  $zipDest = Join-Path $outDirResolved "aero-guest-tools.zip"
  $manifestDest = Join-Path $outDirResolved "aero-guest-tools.manifest.json"

  foreach ($p in @($isoDest, $zipDest, $manifestDest)) {
    if (Test-Path -LiteralPath $p) {
      Remove-Item -LiteralPath $p -Force
    }
  }

  Move-Item -LiteralPath $isoSrc -Destination $isoDest -Force
  Move-Item -LiteralPath $zipSrc -Destination $zipDest -Force
  Move-Item -LiteralPath $manifestSrc -Destination $manifestDest -Force

  $success = $true
} finally {
  if ($success -and (Test-Path -LiteralPath $stageRoot)) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

Write-Host "Guest Tools artifacts created in '$outDirResolved':"
Write-Host "  aero-guest-tools.iso"
Write-Host "  aero-guest-tools.zip"
Write-Host "  aero-guest-tools.manifest.json"

