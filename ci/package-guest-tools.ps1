#Requires -Version 5.1
#
# CI wrapper around the deterministic Rust Guest Tools packager.
#
# Inputs:
#   - Signed driver packages (typically `out/packages/**/<arch>/...`)
#   - The signing certificate used for the driver catalogs (typically `out/certs/aero-test.cer`)
#   - Guest Tools scripts/config (`guest-tools/`)
#
# Outputs (in -OutDir):
#   - aero-guest-tools.iso
#   - aero-guest-tools.zip
#   - manifest.json

[CmdletBinding()]
param(
  [string] $InputRoot = "out/packages",
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

  [string] $SpecPath = "tools/packaging/specs/win7-aero-guest-tools.json",
  [string] $OutDir = "out/artifacts/guest-tools",
  [string] $Version,
  [string] $BuildId,
  [Nullable[long]] $SourceDateEpoch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoPath {
  param([Parameter(Mandatory = $true)][string] $Path)

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
  return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string] $Path)

  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Require-Command {
  param([Parameter(Mandatory = $true)][string] $Name)

  $cmd = Get-Command $Name -ErrorAction SilentlyContinue
  if (-not $cmd) {
    throw "Required tool not found on PATH: $Name"
  }
  return $cmd.Source
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

function Get-BuildIdString {
  $candidates = @(
    $env:GITHUB_RUN_ID,
    $env:GITHUB_RUN_NUMBER,
    $env:BUILD_BUILDID,
    $env:CI_PIPELINE_ID
  )
  foreach ($c in $candidates) {
    if (-not [string]::IsNullOrWhiteSpace($c)) {
      return ([string]$c).Trim()
    }
  }

  $sha = Try-GetGitValue -Args @("rev-parse", "--short=12", "HEAD")
  if (-not [string]::IsNullOrWhiteSpace($sha)) {
    return $sha
  }

  return "local"
}

function Get-DefaultSourceDateEpoch {
  if ($PSBoundParameters.ContainsKey("SourceDateEpoch") -and $null -ne $SourceDateEpoch) {
    return [long] $SourceDateEpoch
  }

  if (-not [string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
    try {
      return [long] $env:SOURCE_DATE_EPOCH
    } catch {
      # fall through
    }
  }

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

function Get-PackagerArchFromPath {
  param([Parameter(Mandatory = $true)][string] $Path)

  $p = $Path.ToLowerInvariant()

  if ($p -match '(^|[\\/])(amd64|x64|x86_64)([\\/]|$)') {
    return "amd64"
  }

  if ($p -match '(^|[\\/])(x86|i386|win32)([\\/]|$)') {
    return "x86"
  }

  return $null
}

function Get-DriverNameFromRelativeSegments {
  param(
    [Parameter(Mandatory = $true)][string[]] $Segments,
    [Parameter(Mandatory = $true)][string] $Fallback
  )

  $skip = @(
    "drivers",
    "driver",
    "packages",
    "package",
    "signed",
    "release",
    "debug",
    "build",
    "bin",
    "dist",
    "windows",
    "wdk",
    "win7",
    "w7",
    "windows7",
    "win7sp1",
    "sp1",
    "packaging"
  )

  for ($i = $Segments.Count - 1; $i -ge 0; $i--) {
    $segment = $Segments[$i]
    $s = $segment.ToLowerInvariant()
    if ($s -in @("x86", "i386", "win32", "x64", "amd64", "x86_64")) {
      continue
    }
    if ($s -in $skip) {
      continue
    }

    return $segment
  }

  return $Fallback
}

function Normalize-PathComponent {
  param([Parameter(Mandatory = $true)][string] $Value)

  $invalid = [System.IO.Path]::GetInvalidFileNameChars()
  foreach ($c in $invalid) {
    $Value = $Value.Replace($c, "_")
  }
  return $Value
}

function Get-DevicesCmdVariable {
  param(
    [Parameter(Mandatory = $true)][string] $DevicesCmdPath,
    [Parameter(Mandatory = $true)][string] $VarName
  )

  $lines = Get-Content -LiteralPath $DevicesCmdPath -ErrorAction Stop
  foreach ($rawLine in $lines) {
    if ($null -eq $rawLine) { continue }
    $trim = ([string]$rawLine).Trim()
    if ($trim.Length -eq 0) { continue }
    $lower = $trim.ToLowerInvariant()
    if ($lower.StartsWith("rem") -or $lower.StartsWith("::") -or $lower.StartsWith("@echo")) {
      continue
    }

    $m = [regex]::Match($rawLine, "(?i)^\\s*set\\s+(.+?)\\s*$")
    if (-not $m.Success) { continue }

    $rest = $m.Groups[1].Value.Trim()
    if ($rest.StartsWith('"') -and $rest.EndsWith('"') -and $rest.Length -ge 2) {
      $rest = $rest.Substring(1, $rest.Length - 2)
    }

    $eq = $rest.IndexOf("=")
    if ($eq -lt 0) { continue }

    $name = $rest.Substring(0, $eq).Trim()
    $value = $rest.Substring($eq + 1).Trim()

    if ($name -ieq $VarName) {
      return $value
    }
  }

  return $null
}

function Parse-CmdQuotedList {
  param([Parameter(Mandatory = $true)][string] $Value)

  $matches = [regex]::Matches($Value, '"([^"]+)"')
  if ($matches.Count -gt 0) {
    $out = @()
    foreach ($m in $matches) {
      $out += $m.Groups[1].Value
    }
    return ,$out
  }

  $t = $Value.Trim()
  if ($t.Length -eq 0) {
    return @()
  }
  return @($t)
}

function Get-InfAddServiceNames {
  param([Parameter(Mandatory = $true)][string] $InfPath)

  $content = $null
  try {
    $content = Get-Content -LiteralPath $InfPath -Raw -ErrorAction Stop
  } catch {
    return @()
  }

  $names = @{}
  foreach ($line in ($content -split "`r?`n")) {
    $m = [regex]::Match($line, "(?i)^\\s*AddService\\s*=\\s*(.+)$")
    if (-not $m.Success) { continue }

    $rest = $m.Groups[1].Value.Trim()
    if ($rest.Length -eq 0) { continue }
    $rest = $rest.Replace('"', '')

    $svc = $null
    $m2 = [regex]::Match($rest, "^([^,\\s]+)")
    if ($m2.Success) {
      $svc = $m2.Groups[1].Value.Trim()
    }
    if ([string]::IsNullOrWhiteSpace($svc)) { continue }

    $key = $svc.ToLowerInvariant()
    if (-not $names.ContainsKey($key)) {
      $names[$key] = $svc
    }
  }

  return ,($names.Values | Sort-Object)
}

function Update-DevicesCmdStorageServiceFromDrivers {
  param(
    [Parameter(Mandatory = $true)][string] $StageDriversRoot,
    [Parameter(Mandatory = $true)][string] $DevicesCmdPath
  )

  if (-not (Test-Path -LiteralPath $DevicesCmdPath -PathType Leaf)) {
    throw "devices.cmd not found: $DevicesCmdPath"
  }

  $currentService = Get-DevicesCmdVariable -DevicesCmdPath $DevicesCmdPath -VarName "AERO_VIRTIO_BLK_SERVICE"
  $hwidsRaw = Get-DevicesCmdVariable -DevicesCmdPath $DevicesCmdPath -VarName "AERO_VIRTIO_BLK_HWIDS"
  if ([string]::IsNullOrWhiteSpace($hwidsRaw)) {
    Write-Warning "AERO_VIRTIO_BLK_HWIDS is not set in devices.cmd; skipping virtio-blk service auto-detection."
    return
  }
  $hwids = Parse-CmdQuotedList -Value $hwidsRaw
  if (-not $hwids -or $hwids.Count -eq 0) {
    Write-Warning "AERO_VIRTIO_BLK_HWIDS is empty in devices.cmd; skipping virtio-blk service auto-detection."
    return
  }

  $infFiles = @(Get-ChildItem -LiteralPath $StageDriversRoot -Recurse -File -Filter "*.inf" -ErrorAction SilentlyContinue)
  if (-not $infFiles -or $infFiles.Count -eq 0) {
    Write-Warning "No .inf files found under staged drivers root ($StageDriversRoot); skipping virtio-blk service auto-detection."
    return
  }

  $candidateInfs = @()
  $serviceToInfs = @{}
  foreach ($inf in $infFiles) {
    $text = $null
    try {
      $text = Get-Content -LiteralPath $inf.FullName -Raw -ErrorAction Stop
    } catch {
      continue
    }
    $lower = $text.ToLowerInvariant()

    $matchesHwid = $false
    foreach ($hwid in $hwids) {
      if ([string]::IsNullOrWhiteSpace($hwid)) { continue }
      if ($lower.Contains($hwid.ToLowerInvariant())) {
        $matchesHwid = $true
        break
      }
    }
    if (-not $matchesHwid) { continue }

    $candidateInfs += $inf.FullName
    foreach ($svc in (Get-InfAddServiceNames -InfPath $inf.FullName)) {
      $k = $svc.ToLowerInvariant()
      if (-not $serviceToInfs.ContainsKey($k)) {
        $serviceToInfs[$k] = New-Object System.Collections.Generic.List[string]
      }
      [void]$serviceToInfs[$k].Add($inf.FullName)
    }
  }

  if ($serviceToInfs.Count -eq 0) {
    if ($candidateInfs.Count -gt 0) {
      Write-Warning "Found virtio-blk-matching INF(s) but no AddService lines were detected; leaving AERO_VIRTIO_BLK_SERVICE unchanged."
    }
    return
  }

  $serviceCandidates = @($serviceToInfs.Keys | Sort-Object)

  $selected = $null
  if (-not [string]::IsNullOrWhiteSpace($currentService)) {
    $k = $currentService.ToLowerInvariant()
    if ($serviceToInfs.ContainsKey($k)) {
      $selected = $currentService
    }
  }

  if (-not $selected) {
    if ($serviceCandidates.Count -eq 1) {
      $selected = $serviceCandidates[0]
    } else {
      # Disambiguate using the presence of <service>.sys in the staged driver tree.
      $matching = @()
      foreach ($k in $serviceCandidates) {
        $sysName = "$k.sys"
        $hit = Get-ChildItem -LiteralPath $StageDriversRoot -Recurse -File -Filter $sysName -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($hit) { $matching += $k }
      }
      if ($matching.Count -eq 1) {
        $selected = $matching[0]
      } else {
        $details = @()
        foreach ($k in $serviceCandidates) {
          $infs = $serviceToInfs[$k] | Sort-Object -Unique
          $details += ("- {0}: {1}" -f $k, ($infs -join ", "))
        }
        throw "Unable to determine a unique virtio-blk storage service name from staged driver INFs. Candidates: $($serviceCandidates -join ', ').`nINF matches:`n$($details -join \"`n\")"
      }
    }
  }

  if ($selected -and $currentService -and ($selected.ToLowerInvariant() -eq $currentService.ToLowerInvariant())) {
    Write-Host "  AERO_VIRTIO_BLK_SERVICE already matches staged storage driver: $currentService"
    return
  }

  if (-not $selected) {
    return
  }

  Write-Host "  Updating staged Guest Tools config: AERO_VIRTIO_BLK_SERVICE=$selected"

  $lines = Get-Content -LiteralPath $DevicesCmdPath -ErrorAction Stop
  $updated = @()
  $replaced = $false
  foreach ($line in $lines) {
    if ($line -match '(?i)^\\s*set\\s+\"AERO_VIRTIO_BLK_SERVICE=') {
      $updated += ('set "AERO_VIRTIO_BLK_SERVICE={0}"' -f $selected)
      $replaced = $true
      continue
    }
    if ($line -match '(?i)^\\s*set\\s+AERO_VIRTIO_BLK_SERVICE=') {
      $updated += ('set "AERO_VIRTIO_BLK_SERVICE={0}"' -f $selected)
      $replaced = $true
      continue
    }
    $updated += $line
  }
  if (-not $replaced) {
    $updated += ('set "AERO_VIRTIO_BLK_SERVICE={0}"' -f $selected)
  }

  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllLines($DevicesCmdPath, $updated, $utf8NoBom)
}

function Copy-DriversToPackagerLayout {
  param(
    [Parameter(Mandatory = $true)][string] $InputRoot,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  $inputRootTrimmed = $InputRoot.TrimEnd("\", "/")

  $infFiles = Get-ChildItem -Path $InputRoot -Recurse -File -Filter "*.inf" -ErrorAction SilentlyContinue | Sort-Object -Property FullName
  if (-not $infFiles) {
    throw "No '.inf' files found under '$InputRoot'."
  }

  $seen = New-Object "System.Collections.Generic.HashSet[string]"
  $copied = 0

  foreach ($inf in $infFiles) {
    $arch = Get-PackagerArchFromPath -Path $inf.FullName
    if (-not $arch) {
      continue
    }

    $srcDir = Split-Path -Parent $inf.FullName
    $relative = ""
    if ($srcDir.Length -ge $inputRootTrimmed.Length -and $srcDir.StartsWith($inputRootTrimmed, [System.StringComparison]::OrdinalIgnoreCase)) {
      $relative = $srcDir.Substring($inputRootTrimmed.Length).TrimStart("\", "/")
    }
    $segments = @()
    if (-not [string]::IsNullOrWhiteSpace($relative)) {
      $segments = $relative -split "[\\/]+"
    }

    $driverName = Get-DriverNameFromRelativeSegments -Segments $segments -Fallback $inf.BaseName
    $driverName = $driverName.Trim()
    if ([string]::IsNullOrWhiteSpace($driverName)) {
      $driverName = $inf.BaseName
    }
    $driverName = (Normalize-PathComponent -Value $driverName).ToLowerInvariant()

    $key = "$arch|$driverName|$srcDir"
    if ($seen.Contains($key)) {
      continue
    }
    $null = $seen.Add($key)

    $destDir = Join-Path $StageDriversRoot (Join-Path $arch $driverName)
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item -Path (Join-Path $srcDir "*") -Destination $destDir -Recurse -Force
    $copied++
  }

  if ($copied -eq 0) {
    throw "No driver packages found under '$InputRoot' for known architectures (x86/amd64)."
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
      $dst = Join-Path $destArchDir $d.Name.ToLowerInvariant()
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
    $driverName = $d.Name.ToLowerInvariant()

    $srcX86 = Join-Path $d.FullName "x86"
    if (Test-Path -LiteralPath $srcX86 -PathType Container) {
      $dst = Join-Path $destX86 $driverName
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
      $dst = Join-Path $destAmd64 $driverName
      New-Item -ItemType Directory -Force -Path $dst | Out-Null
      Copy-Item -Path (Join-Path $srcX64 "*") -Destination $dst -Recurse -Force
    }
  }
}

function Stage-GuestTools {
  param(
    [Parameter(Mandatory = $true)][string] $SourceDir,
    [Parameter(Mandatory = $true)][string] $DestDir,
    [Parameter(Mandatory = $true)][string] $CertSourcePath,
    [Parameter(Mandatory = $true)][bool] $IncludeCerts
  )

  Ensure-EmptyDirectory -Path $DestDir
  Copy-Item -Path (Join-Path $SourceDir "*") -Destination $DestDir -Recurse -Force

  $certsDir = Join-Path $DestDir "certs"
  if (-not (Test-Path -LiteralPath $certsDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $certsDir | Out-Null
  }

  $existing = @(Get-ChildItem -LiteralPath $certsDir -Recurse -File -ErrorAction SilentlyContinue)
  foreach ($file in $existing) {
    if ($file.Name -ieq "README.md") {
      continue
    }
    if ($file.Extension.ToLowerInvariant() -in @(".cer", ".crt", ".p7b")) {
      Remove-Item -LiteralPath $file.FullName -Force
    }
  }

  if ($IncludeCerts) {
    $certDest = Join-Path $certsDir (Split-Path -Leaf $CertSourcePath)
    Copy-Item -LiteralPath $CertSourcePath -Destination $certDest -Force
  }
}

function Assert-FileExistsNonEmpty {
  param([Parameter(Mandatory = $true)][string] $Path)

  $file = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
  if (-not $file -or $file.Length -le 0) {
    throw "Expected output file to exist and be non-empty: '$Path'."
  }
}

function Assert-ZipContainsFile {
  param(
    [Parameter(Mandatory = $true)][string] $ZipPath,
    [Parameter(Mandatory = $true)][string] $EntryPath
  )

  Add-Type -AssemblyName System.IO.Compression
  $fs = [System.IO.File]::OpenRead($ZipPath)
  $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
  try {
    $match = $zip.Entries | Where-Object { $_.FullName -eq $EntryPath } | Select-Object -First 1
    if (-not $match) {
      throw "Expected ZIP '$ZipPath' to contain entry '$EntryPath'."
    }
  } finally {
    $zip.Dispose()
    $fs.Dispose()
  }
}

$inputRootResolved = Resolve-RepoPath -Path $InputRoot
$guestToolsResolved = Resolve-RepoPath -Path $GuestToolsDir
$certPathResolved = Resolve-RepoPath -Path $CertPath
$specPathResolved = Resolve-RepoPath -Path $SpecPath
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
if (-not (Test-Path -LiteralPath $specPathResolved -PathType Leaf)) {
  throw "SpecPath does not exist: '$specPathResolved'."
}

if ([string]::IsNullOrWhiteSpace($Version)) {
  $Version = Get-VersionString
}
if ([string]::IsNullOrWhiteSpace($BuildId)) {
  $BuildId = Get-BuildIdString
}

$epoch = Get-DefaultSourceDateEpoch

Require-Command -Name "cargo" | Out-Null

$packagerManifest = Resolve-RepoPath -Path "tools/packaging/aero_packager/Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Missing packager Cargo.toml: '$packagerManifest'."
}

$stageRoot = Resolve-RepoPath -Path "out/_staging_guest_tools"
$stageDriversRoot = Join-Path $stageRoot "drivers"
$stageGuestTools = Join-Path $stageRoot "guest-tools"
$stageInputExtract = Join-Path $stageRoot "input"

$success = $false
try {
  Ensure-EmptyDirectory -Path $stageRoot
  Ensure-EmptyDirectory -Path $stageDriversRoot
  Ensure-EmptyDirectory -Path $stageInputExtract

  Write-Host "Staging Guest Tools..."
  $includeCerts = $SigningPolicy -ine "none"
  Stage-GuestTools -SourceDir $guestToolsResolved -DestDir $stageGuestTools -CertSourcePath $certPathResolved -IncludeCerts:$includeCerts

  Write-Host "Staging signed drivers..."

  $inputRootForStaging = $inputRootResolved
  if (Test-Path -LiteralPath $inputRootResolved -PathType Leaf) {
    $inputExt = [System.IO.Path]::GetExtension($inputRootResolved).ToLowerInvariant()
    if ($inputExt -eq ".zip") {
      Write-Host "  Extracting driver bundle ZIP: $inputRootResolved"
      Expand-Archive -LiteralPath $inputRootResolved -DestinationPath $stageInputExtract -Force

      $topDirs = @(Get-ChildItem -LiteralPath $stageInputExtract -Directory -ErrorAction SilentlyContinue)
      $topFiles = @(Get-ChildItem -LiteralPath $stageInputExtract -File -ErrorAction SilentlyContinue)
      if ($topDirs.Count -eq 1 -and $topFiles.Count -eq 0) {
        $inputRootForStaging = $topDirs[0].FullName
      } else {
        $inputRootForStaging = $stageInputExtract
      }
    } else {
      throw "InputRoot is a file with unsupported extension '$inputExt'. Expected a directory, or a .zip."
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
    Copy-DriversToPackagerLayout -InputRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  }

  # Ensure the staged Guest Tools config matches the staged storage driver packages.
  # (setup.cmd validates AERO_VIRTIO_BLK_SERVICE against INF AddService names.)
  $devicesCmdStage = Join-Path (Join-Path $stageGuestTools "config") "devices.cmd"
  if (Test-Path -LiteralPath $devicesCmdStage -PathType Leaf) {
    Update-DevicesCmdStorageServiceFromDrivers -StageDriversRoot $stageDriversRoot -DevicesCmdPath $devicesCmdStage
  }

  New-Item -ItemType Directory -Force -Path $outDirResolved | Out-Null

  Write-Host "Packaging via aero_packager..."
  Write-Host "  version : $Version"
  Write-Host "  build-id: $BuildId"
  Write-Host "  epoch   : $epoch"
  Write-Host "  policy  : $SigningPolicy"
  Write-Host "  spec    : $specPathResolved"
  Write-Host "  out     : $outDirResolved"

  & cargo run --manifest-path $packagerManifest --release --locked -- `
    --drivers-dir $stageDriversRoot `
    --guest-tools-dir $stageGuestTools `
    --spec $specPathResolved `
    --out-dir $outDirResolved `
    --version $Version `
    --build-id $BuildId `
    --signing-policy $SigningPolicy `
    --source-date-epoch $epoch
  if ($LASTEXITCODE -ne 0) {
    throw "aero_packager failed (exit code $LASTEXITCODE)."
  }

  $isoPath = Join-Path $outDirResolved "aero-guest-tools.iso"
  $zipPath = Join-Path $outDirResolved "aero-guest-tools.zip"
  $manifestPath = Join-Path $outDirResolved "manifest.json"

  Assert-FileExistsNonEmpty -Path $isoPath
  Assert-FileExistsNonEmpty -Path $zipPath
  Assert-FileExistsNonEmpty -Path $manifestPath

  if ($includeCerts) {
    $certLeaf = Split-Path -Leaf $certPathResolved
    Assert-ZipContainsFile -ZipPath $zipPath -EntryPath ("certs/{0}" -f $certLeaf)
  }

  $success = $true
} finally {
  if ($success -and (Test-Path -LiteralPath $stageRoot)) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

Write-Host "Guest Tools artifacts created in '$outDirResolved':"
Write-Host "  aero-guest-tools.iso"
Write-Host "  aero-guest-tools.zip"
Write-Host "  manifest.json"
