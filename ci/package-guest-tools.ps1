#Requires -Version 5.1
#
# CI wrapper around the deterministic Rust Guest Tools packager.
#
# Inputs:
#   - Signed driver packages (typically `out/packages/**/<arch>/...`)
#   - Optional signing certificate used for the driver catalogs (typically `out/certs/aero-test.cer`;
#     required when `-SigningPolicy` resolves to `test`)
#   - Guest Tools scripts/config (`guest-tools/`)
#
# Outputs (in -OutDir):
#   - aero-guest-tools.iso
#   - aero-guest-tools.zip
#   - manifest.json (raw packager output)
#   - aero-guest-tools.manifest.json (copy of manifest.json; avoids collisions in shared artifact dirs)
#
# The packaged ISO/zip root includes `THIRD_PARTY_NOTICES.md` and may include additional
# third-party license/notice texts under `licenses/` when present in the source Guest Tools tree.

[CmdletBinding()]
param(
  [string] $InputRoot = "out/packages",
  [string] $GuestToolsDir = "guest-tools",

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
  [string] $SigningPolicy = "test",

  # Public certificate used to sign the driver catalogs (required when SigningPolicy=test).
  [string] $CertPath = "out/certs/aero-test.cer",

  [string] $SpecPath = "tools/packaging/specs/win7-aero-guest-tools.json",
  [string] $DriverNameMapJson,
  [string] $OutDir = "out/artifacts",
  [string] $Version,
  [string] $BuildId,
  [Nullable[long]] $SourceDateEpoch,

  # Machine-readable device contract used to generate the packaged config/devices.cmd.
  #
  # Defaults to the canonical Aero device contract, but callers that package upstream
  # virtio-win drivers may need to override service names (e.g. viostor/netkvm).
  [string] $WindowsDeviceContractPath = "docs/windows-device-contract.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Optional driver rename overrides loaded from -DriverNameMapJson.
# Keys may be either:
# - a driverRel (relative path under out/packages, e.g. "windows7/virtio/blk"), or
# - a leaf driver folder name (e.g. "blk")
$script:DriverNameMap = @{}

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

function Normalize-SigningPolicy {
  param([Parameter(Mandatory = $true)][string] $Policy)

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

function Normalize-DriverRel {
  param([Parameter(Mandatory = $true)][string] $Value)

  $normalized = $Value.Replace([System.IO.Path]::DirectorySeparatorChar, "/").Replace([System.IO.Path]::AltDirectorySeparatorChar, "/")
  $normalized = ($normalized -replace "/+", "/").Trim("/")
  return $normalized
}

function Load-DriverNameMap {
  param([string] $Path)

  $map = @{}
  if ([string]::IsNullOrWhiteSpace($Path)) {
    return $map
  }

  $resolved = Resolve-RepoPath -Path $Path
  if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
    throw "DriverNameMapJson does not exist: '$resolved'."
  }

  $raw = Get-Content -LiteralPath $resolved -Raw
  if ([string]::IsNullOrWhiteSpace($raw)) {
    throw "DriverNameMapJson is empty: '$resolved'."
  }

  $obj = $raw | ConvertFrom-Json
  if ($null -eq $obj) {
    throw "DriverNameMapJson did not parse as JSON: '$resolved'."
  }

  foreach ($prop in $obj.PSObject.Properties) {
    $kRaw = ([string]$prop.Name).Trim()
    $vRaw = ([string]$prop.Value).Trim()
    if ([string]::IsNullOrWhiteSpace($kRaw) -or [string]::IsNullOrWhiteSpace($vRaw)) {
      continue
    }

    $key = (Normalize-DriverRel -Value $kRaw).ToLowerInvariant()
    $map[$key] = $vRaw
  }

  return $map
}

function Get-GuestToolsDriverNameFromDriverRel {
  param([Parameter(Mandatory = $true)][string] $DriverRel)

  # Map `<driverRel>` (from `out/packages/<driverRel>/<arch>/...`) to a stable,
  # Guest Tools-facing directory name.
  #
  # Default: last path segment of <driverRel>
  # Overrides: known non-leaf driverRel where the leaf is ambiguous.
  $overrides = @{
    "windows7/virtio/blk" = "virtio-blk"
    "windows7/virtio/net" = "virtio-net"
    # Support staging from historical layouts that used a dash in the driver directory name.
    "aero-gpu"            = "aerogpu"
  }

  $relNorm = Normalize-DriverRel -Value $DriverRel
  $key = $relNorm.ToLowerInvariant()
  if ($script:DriverNameMap.ContainsKey($key)) {
    return $script:DriverNameMap[$key]
  }
  if ($overrides.ContainsKey($key)) {
    return $overrides[$key]
  }

  $parts = @($relNorm -split "/+")
  if (-not $parts -or $parts.Count -eq 0) {
    throw "Invalid driverRel: '$DriverRel'"
  }
  return $parts[$parts.Count - 1]
}

function Normalize-GuestToolsDriverName {
  param([Parameter(Mandatory = $true)][string] $Name)

  $normalized = (Normalize-PathComponent -Value $Name.Trim()).ToLowerInvariant()
  if ($script:DriverNameMap.ContainsKey($normalized)) {
    return (Normalize-PathComponent -Value $script:DriverNameMap[$normalized]).ToLowerInvariant()
  }
  $overrides = @{
    # Support staging from historical layouts that used a dash in the AeroGPU directory name.
    "aero-gpu" = "aerogpu"
    # Support staging from legacy driver bundle layouts (ci/package-drivers.ps1) that use leaf names.
    "blk"      = "virtio-blk"
    "net"      = "virtio-net"
  }
  if ($overrides.ContainsKey($normalized)) {
    return $overrides[$normalized]
  }
  return $normalized
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

function Update-WindowsDeviceContractStorageServiceFromDrivers {
  param(
    [Parameter(Mandatory = $true)][string] $ContractPath,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  if (-not (Test-Path -LiteralPath $ContractPath -PathType Leaf)) {
    throw "Windows device contract JSON not found: $ContractPath"
  }

  $contract = Get-Content -LiteralPath $ContractPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
  if (-not $contract -or -not $contract.devices) {
    throw "Windows device contract JSON is missing the required 'devices' field: $ContractPath"
  }

  $virtioBlk = $null
  foreach ($d in $contract.devices) {
    if ($d -and $d.device -and ($d.device -ieq "virtio-blk")) {
      $virtioBlk = $d
      break
    }
  }
  if (-not $virtioBlk) {
    throw "Windows device contract JSON does not contain a virtio-blk entry to patch: $ContractPath"
  }

  $currentService = ("" + $virtioBlk.driver_service_name).Trim()
  $hwids = @()
  foreach ($p in $virtioBlk.hardware_id_patterns) {
    $t = ("" + $p).Trim()
    if ($t.Length -gt 0) { $hwids += $t }
  }
  if (-not $hwids -or $hwids.Count -eq 0) {
    Write-Warning "virtio-blk entry has no hardware_id_patterns in $ContractPath; skipping storage service auto-detection."
    return
  }

  $infFiles = @(
    Get-ChildItem -LiteralPath $StageDriversRoot -Recurse -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -match '(?i)\\.inf$' }
  )
  if (-not $infFiles -or $infFiles.Count -eq 0) {
    Write-Warning "No .inf files found under staged drivers root ($StageDriversRoot); skipping storage service auto-detection."
    return
  }

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
      if ($lower.Contains($hwid.ToLowerInvariant())) {
        $matchesHwid = $true
        break
      }
    }
    if (-not $matchesHwid) { continue }

    foreach ($svc in (Get-InfAddServiceNames -InfPath $inf.FullName)) {
      $k = $svc.ToLowerInvariant()
      if (-not $serviceToInfs.ContainsKey($k)) {
        $serviceToInfs[$k] = New-Object System.Collections.Generic.List[string]
      }
      [void]$serviceToInfs[$k].Add($inf.FullName)
    }
  }

  if ($serviceToInfs.Count -eq 0) {
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
        $hit = Get-ChildItem -LiteralPath $StageDriversRoot -Recurse -File -ErrorAction SilentlyContinue |
          Where-Object { $_.Name -ieq $sysName } |
          Select-Object -First 1
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

  if (-not $selected) { return }
  if ($selected -and $currentService -and ($selected.ToLowerInvariant() -eq $currentService.ToLowerInvariant())) {
    return
  }

  Write-Host "Patching Windows device contract: virtio-blk driver_service_name=$selected"
  $virtioBlk.driver_service_name = $selected
  $json = $contract | ConvertTo-Json -Depth 20
  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllText($ContractPath, ($json + "`n"), $utf8NoBom)
}

function Copy-DriversToPackagerLayout {
  param(
    [Parameter(Mandatory = $true)][string] $InputRoot,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot
  )

  $inputRootTrimmed = $InputRoot.TrimEnd("\", "/")
  $expectedCiPackages = (Resolve-RepoPath -Path "out/packages").TrimEnd("\", "/")
  $enforceCiManifestGate = $inputRootTrimmed -ieq $expectedCiPackages
  $driversSrcRoot = $null
  if ($enforceCiManifestGate) {
    $driversSrcRoot = Resolve-RepoPath -Path "drivers"
  }

  # CI packages layout produced by `ci/make-catalogs.ps1`:
  #   out/packages/<driverRel>/{x86,x64}/...
  #
  # Identify driver roots by scanning for directories containing architecture subdirectories.
  $archDirNames = @("x86", "x64", "amd64", "win32", "i386", "x86_64", "x86-64")
  $driverRoots = New-Object System.Collections.Generic.List[string]
  $seenRoots = @{}

  # Include InputRoot itself in case it directly contains architecture subdirectories
  # (e.g. when staging from a single extracted driver root).
  $dirs = @()
  try {
    $dirs += (Get-Item -LiteralPath $InputRoot -ErrorAction Stop)
  } catch {
    # fall through; Get-ChildItem below will surface the missing path
  }
  $dirs += @(Get-ChildItem -LiteralPath $InputRoot -Directory -Recurse -ErrorAction SilentlyContinue)
  foreach ($dir in $dirs) {
    $children = @(Get-ChildItem -LiteralPath $dir.FullName -Directory -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
    if (-not $children -or $children.Count -eq 0) { continue }

    $hasArchChild = $false
    foreach ($child in $children) {
      if ($archDirNames -contains $child) {
        $hasArchChild = $true
        break
      }
    }
    if (-not $hasArchChild) { continue }

    $k = $dir.FullName.ToLowerInvariant()
    if ($seenRoots.ContainsKey($k)) { continue }
    $seenRoots[$k] = $true
    [void]$driverRoots.Add($dir.FullName)
  }

  if ($driverRoots.Count -eq 0) {
    throw "No driver packages found under '$InputRoot'. Expected: out/packages/<driverRel>/{x86,x64}/..."
  }

  $nameToRel = @{}
  $stagedArchKeys = @{}

  foreach ($driverRoot in ($driverRoots | Sort-Object)) {
    $relative = ""
    if ($driverRoot.Length -ge $inputRootTrimmed.Length -and $driverRoot.StartsWith($inputRootTrimmed, [System.StringComparison]::OrdinalIgnoreCase)) {
      $relative = $driverRoot.Substring($inputRootTrimmed.Length).TrimStart("\", "/")
    } else {
      $relative = Split-Path -Leaf $driverRoot
    }
    if ([string]::IsNullOrWhiteSpace($relative)) {
      $relative = Split-Path -Leaf $driverRoot
    }

    $driverRel = Normalize-DriverRel -Value $relative

    if ($enforceCiManifestGate) {
      $driverRelForPath = $driverRel.Replace("/", [System.IO.Path]::DirectorySeparatorChar)
      $driverSourceDir = Join-Path $driversSrcRoot $driverRelForPath
      $manifestPath = Join-Path $driverSourceDir "ci-package.json"
      if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "Refusing to package Guest Tools: driver package '$driverRel' is missing required manifest '$manifestPath'."
      }
    }

    $driverName = Get-GuestToolsDriverNameFromDriverRel -DriverRel $driverRel
    $driverName = (Normalize-PathComponent -Value $driverName).ToLowerInvariant()

    $nameKey = $driverName.ToLowerInvariant()
    if ($nameToRel.ContainsKey($nameKey) -and $nameToRel[$nameKey] -ne $driverRel) {
      throw "Driver name collision: '$driverName' maps from both '$($nameToRel[$nameKey])' and '$driverRel'. Add an explicit override mapping to disambiguate."
    }
    $nameToRel[$nameKey] = $driverRel

    Write-Host ("  Staging driver: {0} -> {1}" -f $driverRel, $driverName)

    $archDirs = @(Get-ChildItem -LiteralPath $driverRoot -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name)
    foreach ($archDir in $archDirs) {
      $arch = Get-PackagerArchFromPath -Path $archDir.Name
      if (-not $arch) { continue }

      $archKey = ("{0}|{1}" -f $driverName, $arch)
      if ($stagedArchKeys.ContainsKey($archKey)) {
        throw "Duplicate arch mapping for driver '$driverName': '$($stagedArchKeys[$archKey])' and '$($archDir.Name)' both map to '$arch'."
      }
      $stagedArchKeys[$archKey] = $archDir.Name

      $destDir = Join-Path $StageDriversRoot (Join-Path $arch $driverName)
      New-Item -ItemType Directory -Force -Path $destDir | Out-Null
      Copy-Item -Path (Join-Path $archDir.FullName "*") -Destination $destDir -Recurse -Force
    }
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
      $dst = Join-Path $destArchDir (Normalize-GuestToolsDriverName -Name $d.Name)
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
    $driverName = Normalize-GuestToolsDriverName -Name $d.Name

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

function Show-PackagerHwidDiagnostics {
  param(
    [Parameter(Mandatory = $true)][string] $StageDriversRoot,
    [Parameter(Mandatory = $true)][string] $SpecPath,
    [string] $WindowsDeviceContractPath = ""
  )

  Write-Host ""
  Write-Host "---- Packager HWID diagnostics (staged driver INFs) ----"
  Write-Host ("  drivers  : {0}" -f $StageDriversRoot)
  Write-Host ("  spec     : {0}" -f $SpecPath)
  if (-not [string]::IsNullOrWhiteSpace($WindowsDeviceContractPath)) {
    Write-Host ("  contract : {0}" -f $WindowsDeviceContractPath)
  }

  if (-not (Test-Path -LiteralPath $StageDriversRoot -PathType Container)) {
    Write-Warning "Staged drivers directory not found; skipping HWID diagnostics."
    return
  }
  if (-not (Test-Path -LiteralPath $SpecPath -PathType Leaf)) {
    Write-Warning "Spec file not found; skipping HWID diagnostics."
    return
  }

  $spec = $null
  try {
    $spec = Get-Content -LiteralPath $SpecPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
  } catch {
    Write-Warning ("Failed to parse spec JSON for diagnostics: {0}" -f $_.Exception.Message)
    return
  }

  $driverEntries = @()
  if ($spec -and $spec.drivers) { $driverEntries += $spec.drivers }
  if ($spec -and $spec.required_drivers) { $driverEntries += $spec.required_drivers }
  if (-not $driverEntries -or $driverEntries.Count -eq 0) {
    Write-Warning "Spec contains no driver entries; skipping HWID diagnostics."
    return
  }

  $contract = $null
  if (-not [string]::IsNullOrWhiteSpace($WindowsDeviceContractPath) -and (Test-Path -LiteralPath $WindowsDeviceContractPath -PathType Leaf)) {
    try {
      $contract = Get-Content -LiteralPath $WindowsDeviceContractPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
    } catch {
      Write-Warning ("Failed to parse Windows device contract for HWID diagnostics: {0}" -f $_.Exception.Message)
      $contract = $null
    }
  }

  function Get-ContractHwidBasesFromDevicesCmdVar {
    param(
      [Parameter(Mandatory = $true)] $ContractObj,
      [Parameter(Mandatory = $true)][string] $VarName
    )

    if (-not $ContractObj -or -not $ContractObj.devices) {
      return @()
    }

    $deviceName = $null
    switch ($VarName.Trim().ToUpperInvariant()) {
      "AERO_VIRTIO_BLK_HWIDS" { $deviceName = "virtio-blk" }
      "AERO_VIRTIO_NET_HWIDS" { $deviceName = "virtio-net" }
      "AERO_VIRTIO_INPUT_HWIDS" { $deviceName = "virtio-input" }
      "AERO_VIRTIO_SND_HWIDS" { $deviceName = "virtio-snd" }
      "AERO_GPU_HWIDS" { $deviceName = "aero-gpu" }
      default { return @() }
    }

    $dev = $null
    foreach ($d in $ContractObj.devices) {
      if ($d -and $d.device -and ($d.device -ieq $deviceName)) {
        $dev = $d
        break
      }
    }
    if (-not $dev -or -not $dev.hardware_id_patterns) {
      return @()
    }

    $bases = @()
    foreach ($p in $dev.hardware_id_patterns) {
      $t = ("" + $p).Trim()
      if ($t.Length -eq 0) { continue }
      $m = [regex]::Match($t, "(?i)(PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4})")
      if ($m.Success) {
        $bases += $m.Groups[1].Value
      }
    }

    # Dedup while preserving order
    $seen = @{}
    $out = @()
    foreach ($b in $bases) {
      $k = $b.ToLowerInvariant()
      if (-not $seen.ContainsKey($k)) {
        $seen[$k] = $true
        $out += $b
      }
    }
    return ,$out
  }

  # Best-effort: when a spec HWID regex doesn't match any line in the INF, print any PCI HWID-like
  # lines present so CI logs make it obvious whether the driver has the wrong DEV_XXXX (e.g. a
  # transitional virtio ID) or is missing hardware IDs entirely.
  $pciLineRegex = [regex]::new("PCI\\VEN_", [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)

  foreach ($drv in $driverEntries) {
    if (-not $drv -or -not $drv.name) { continue }
    $driverName = ("" + $drv.name).Trim()
    if ([string]::IsNullOrWhiteSpace($driverName)) { continue }

    $patterns = @()
    if ($drv.expected_hardware_ids) {
      foreach ($p in $drv.expected_hardware_ids) {
        $t = ("" + $p).Trim()
        if ($t.Length -gt 0) { $patterns += $t }
      }
    }

    # Derive base PCI\VEN_...&DEV_... patterns from the Windows device contract when the spec uses
    # `expected_hardware_ids_from_devices_cmd_var` (used by win7-signed.json and AeroGPU).
    $fromVar = $null
    if ($drv.expected_hardware_ids_from_devices_cmd_var) {
      $fromVar = ("" + $drv.expected_hardware_ids_from_devices_cmd_var).Trim()
    }
    if (-not [string]::IsNullOrWhiteSpace($fromVar) -and $contract) {
      foreach ($base in (Get-ContractHwidBasesFromDevicesCmdVar -ContractObj $contract -VarName $fromVar)) {
        $escaped = [regex]::Escape($base)
        if (-not ($patterns -contains $escaped)) {
          $patterns += $escaped
        }
      }
    }

    if (-not $patterns -or $patterns.Count -eq 0) {
      continue
    }

    $compiled = @()
    foreach ($p in $patterns) {
      try {
        $compiled += [pscustomobject]@{
          Pattern = $p
          Regex   = [regex]::new($p, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
        }
      } catch {
        Write-Warning ("Invalid expected_hardware_ids regex for driver {0}: {1}" -f $driverName, $p)
      }
    }
    if (-not $compiled -or $compiled.Count -eq 0) { continue }

    foreach ($arch in @("x86", "amd64")) {
      $driverDir = Join-Path (Join-Path $StageDriversRoot $arch) $driverName
      if (-not (Test-Path -LiteralPath $driverDir -PathType Container)) {
        continue
      }

      Write-Host ""
      Write-Host ("Driver: {0} ({1})" -f $driverName, $arch)
      if (-not [string]::IsNullOrWhiteSpace($fromVar)) {
        Write-Host ("  (derived from devices.cmd var: {0})" -f $fromVar)
      }
      Write-Host "  Expected HWID regexes:"
      foreach ($p in $patterns) {
        Write-Host ("    - {0}" -f $p)
      }

      $infFiles = @(
        Get-ChildItem -LiteralPath $driverDir -Recurse -File -ErrorAction SilentlyContinue |
          Where-Object { $_.Name -match '(?i)\.inf$' } |
          Sort-Object -Property FullName
      )
      if (-not $infFiles -or $infFiles.Count -eq 0) {
        Write-Host "  (no INF files found)"
        continue
      }

      foreach ($inf in $infFiles) {
        Write-Host ("  INF: {0}" -f $inf.FullName)
        $lines = @()
        try {
          $lines = Get-Content -LiteralPath $inf.FullName -ErrorAction Stop
        } catch {
          Write-Host "    (failed to read INF)"
          continue
        }

        $anyMatch = $false
        $printed = 0
        $maxPrint = 50
        for ($i = 0; $i -lt $lines.Count; $i++) {
          $line = $lines[$i]
          foreach ($entry in $compiled) {
            if ($entry.Regex.IsMatch($line)) {
              if (-not $anyMatch) {
                Write-Host "    Matches:"
              }
              $anyMatch = $true
              $printed += 1
              Write-Host ("      L{0}: {1}" -f ($i + 1), $line.Trim())
              if ($printed -ge $maxPrint) {
                Write-Host ("      ... truncated after {0} match(es) ..." -f $printed)
                break
              }
            }
          }
          if ($printed -ge $maxPrint) { break }
        }

        if (-not $anyMatch) {
          Write-Host "    No matches found. PCI HWID-like lines present in INF:"
          $printed = 0
          for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            if ($pciLineRegex.IsMatch($line)) {
              $printed += 1
              Write-Host ("      L{0}: {1}" -f ($i + 1), $line.Trim())
              if ($printed -ge $maxPrint) {
                Write-Host ("      ... truncated after {0} line(s) ..." -f $printed)
                break
              }
            }
          }
          if ($printed -eq 0) {
            Write-Host "      (none found)"
          }
        }
      }
    }
  }
}

$inputRootResolved = Resolve-RepoPath -Path $InputRoot
$guestToolsResolved = Resolve-RepoPath -Path $GuestToolsDir
$certPathResolved = Resolve-RepoPath -Path $CertPath
$specPathResolved = Resolve-RepoPath -Path $SpecPath
$outDirResolved = Resolve-RepoPath -Path $OutDir

$SigningPolicy = Normalize-SigningPolicy -Policy $SigningPolicy

if (-not (Test-Path -LiteralPath $inputRootResolved)) {
  throw "InputRoot does not exist: '$inputRootResolved'."
}
if (-not (Test-Path -LiteralPath $guestToolsResolved -PathType Container)) {
  throw "GuestToolsDir does not exist: '$guestToolsResolved'."
}
$includeCerts = $SigningPolicy -eq "test"
if ($includeCerts) {
  if (-not (Test-Path -LiteralPath $certPathResolved -PathType Leaf)) {
    throw "CertPath does not exist: '$certPathResolved'."
  }
}
if (-not (Test-Path -LiteralPath $specPathResolved -PathType Leaf)) {
  throw "SpecPath does not exist: '$specPathResolved'."
}

if (-not [string]::IsNullOrWhiteSpace($DriverNameMapJson)) {
  $script:DriverNameMap = Load-DriverNameMap -Path $DriverNameMapJson
  if ($script:DriverNameMap.Count -gt 0) {
    $driverNameMapResolved = Resolve-RepoPath -Path $DriverNameMapJson
    Write-Host ("Loaded driver name overrides: {0} entry(s) from {1}" -f $script:DriverNameMap.Count, $driverNameMapResolved)
  }
}

if ([string]::IsNullOrWhiteSpace($Version)) {
  $Version = Get-VersionString
}
if ([string]::IsNullOrWhiteSpace($BuildId)) {
  $BuildId = Get-BuildIdString
}

$epoch = Get-DefaultSourceDateEpoch

# Propagate the epoch through the environment for any tools/libraries that depend on SOURCE_DATE_EPOCH
# for reproducible timestamps.
$env:SOURCE_DATE_EPOCH = "$epoch"

Require-Command -Name "cargo" | Out-Null

$packagerManifest = Resolve-RepoPath -Path "tools/packaging/aero_packager/Cargo.toml"
if (-not (Test-Path -LiteralPath $packagerManifest -PathType Leaf)) {
  throw "Missing packager Cargo.toml: '$packagerManifest'."
}

$windowsDeviceContractResolved = Resolve-RepoPath -Path $WindowsDeviceContractPath
if (-not (Test-Path -LiteralPath $windowsDeviceContractResolved -PathType Leaf)) {
  throw "Missing Windows device contract JSON: '$windowsDeviceContractResolved'."
}

$stageRoot = Resolve-RepoPath -Path "out/_staging_guest_tools"
$stageDriversRoot = Join-Path $stageRoot "drivers"
$stageGuestTools = Join-Path $stageRoot "guest-tools"
$stageInputExtract = Join-Path $stageRoot "input"
$stageDeviceContract = Join-Path $stageRoot "windows-device-contract.json"

$success = $false
try {
  Ensure-EmptyDirectory -Path $stageRoot
  Ensure-EmptyDirectory -Path $stageDriversRoot
  Ensure-EmptyDirectory -Path $stageInputExtract

  Write-Host "Staging Guest Tools..."
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
    Write-Host "  Detected input layout: driver bundle (drivers/<driver>/(x86|x64)/...)"
    Stage-DriversFromBundleLayout -BundleRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  } else {
    Write-Host "  Detected input layout: CI packages (out/packages/<driver>/<arch>/...)"
    Copy-DriversToPackagerLayout -InputRoot $inputRootForStaging -StageDriversRoot $stageDriversRoot
  }

  # aero_packager generates config/devices.cmd from the Windows device contract.
  # Patch a staged copy of the contract so the packaged media uses the storage service name
  # that matches the staged virtio-blk driver (e.g. viostor for upstream virtio-win bundles).
  Copy-Item -LiteralPath $windowsDeviceContractResolved -Destination $stageDeviceContract -Force
  Update-WindowsDeviceContractStorageServiceFromDrivers -ContractPath $stageDeviceContract -StageDriversRoot $stageDriversRoot

  New-Item -ItemType Directory -Force -Path $outDirResolved | Out-Null

  Write-Host "Packaging via aero_packager..."
  Write-Host "  version : $Version"
  Write-Host "  build-id: $BuildId"
  Write-Host "  epoch   : $epoch"
  Write-Host "  policy  : $SigningPolicy"
  Write-Host "  spec    : $specPathResolved"
  Write-Host "  contract: $stageDeviceContract"
  Write-Host "  out     : $outDirResolved"

  try {
    & cargo run --manifest-path $packagerManifest --release --locked -- `
      --drivers-dir $stageDriversRoot `
      --guest-tools-dir $stageGuestTools `
      --spec $specPathResolved `
      --windows-device-contract $stageDeviceContract `
      --out-dir $outDirResolved `
      --version $Version `
      --build-id $BuildId `
      --signing-policy $SigningPolicy `
      --source-date-epoch $epoch
    if ($LASTEXITCODE -ne 0) {
      throw "aero_packager failed (exit code $LASTEXITCODE)."
    }
  } catch {
    Write-Host ""
    Write-Host "aero_packager failed; collecting HWID diagnostics to aid debugging..."
    try {
      Show-PackagerHwidDiagnostics -StageDriversRoot $stageDriversRoot -SpecPath $specPathResolved -WindowsDeviceContractPath $stageDeviceContract
    } catch {
      Write-Warning ("HWID diagnostics failed: {0}" -f $_.Exception.Message)
    }
    throw
  }

  $isoPath = Join-Path $outDirResolved "aero-guest-tools.iso"
  $zipPath = Join-Path $outDirResolved "aero-guest-tools.zip"
  $manifestPath = Join-Path $outDirResolved "manifest.json"
  $manifestCopyPath = Join-Path $outDirResolved "aero-guest-tools.manifest.json"

  Assert-FileExistsNonEmpty -Path $isoPath
  Assert-FileExistsNonEmpty -Path $zipPath
  Assert-FileExistsNonEmpty -Path $manifestPath

  # Some CI workflows publish release assets from a directory that already contains other
  # artifacts (driver bundle zips, etc). Keep a stable, unique manifest file name so the
  # Guest Tools manifest doesn't clobber any other `manifest.json` that might be present.
  Copy-Item -LiteralPath $manifestPath -Destination $manifestCopyPath -Force
  Assert-FileExistsNonEmpty -Path $manifestCopyPath
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
Write-Host "  aero-guest-tools.manifest.json"
