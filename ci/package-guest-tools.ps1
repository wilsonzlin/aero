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
  # Machine-readable device contract used to generate the packaged `config/devices.cmd`.
  #
  # Defaults to the canonical Aero device contract.
  #
  # If you are packaging upstream virtio-win drivers, prefer the dedicated contract variant:
  #   docs/windows-device-contract-virtio-win.json
  # so the generated `devices.cmd` uses virtio-win service names (viostor/netkvm/vioinput/viosnd)
  # while keeping Aero's emulator-presented PCI IDs/HWID patterns.
  [string] $WindowsDeviceContractPath = "docs/windows-device-contract.json",
  [string] $DriverNameMapJson,
  [string] $OutDir = "out/artifacts",
  [string] $Version,
  [string] $BuildId,
  [Nullable[long]] $SourceDateEpoch,
  # Optional directory whose contents are staged under `guest-tools/tools/` in the packaged
  # ISO/zip. This is intended for CI/local builds that want to ship extra guest-side utilities
  # (e.g. debug/selftest helpers) without checking them into `guest-tools/`.
  [string] $ExtraToolsDir,
  [ValidateSet("merge", "replace")]
  [string] $ExtraToolsDirMode = "merge",
  [switch] $DeterminismSelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Optional driver rename overrides loaded from -DriverNameMapJson.
# Keys may be either:
# - a driverRel (relative path under out/packages, e.g. "windows7/virtio-blk"), or
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

function Test-PrivateKeyExtension {
  param([Parameter(Mandatory = $true)][string] $ExtensionNoDotLower)
 
  # Keep this list aligned with `aero_packager`'s `is_private_key_extension`.
  #
  # We refuse these at staging time (instead of relying on the packager) so CI/local packaging
  # fails fast before any accidental secret material is copied into the staging tree.
  return $ExtensionNoDotLower -in @(
    # Common Windows signing key container formats.
    "pfx", "p12", "pvk", "snk",
    # Common PEM/DER private key encodings.
    "key", "pem", "der",
    # PKCS#8 private key encodings.
    "p8", "pk8",
    # Certificate signing requests may include key-related material and should never ship.
    "csr"
  )
}

function Test-DefaultExcludedToolsExtension {
  param([Parameter(Mandatory = $true)][string] $ExtensionNoDotLower)

  # Keep this list aligned with `aero_packager`'s `is_default_excluded_driver_extension`.
  return $ExtensionNoDotLower -in @(
    # Debug symbols.
    "pdb", "ipdb", "iobj", "dbg", "map", "cod",
    # Build metadata.
    "obj", "lib", "exp", "ilk", "tlog", "log", "tmp", "lastbuildstate", "idb",
    # Source / project files.
    "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "idl", "inl", "rc", "s", "asm",
    "sln", "vcxproj", "props", "targets"
  )
}

function Test-HiddenRelPath {
  param([Parameter(Mandatory = $true)][string] $RelPath)

  $parts = @($RelPath -split '[\\/]')
  foreach ($p in $parts) {
    if ([string]::IsNullOrEmpty($p)) { continue }
    if ($p.StartsWith(".")) { return $true }
    if ($p -eq "__MACOSX") { return $true }
  }
  return $false
}

function Copy-TreeWithSafetyFilters {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)][string] $SourceDir,
    [Parameter(Mandatory = $true)][string] $DestDir
  )

  if (-not (Test-Path -LiteralPath $SourceDir -PathType Container)) {
    throw "ExtraToolsDir does not exist: '$SourceDir'."
  }

  if (-not (Test-Path -LiteralPath $DestDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
  }

  $srcRoot = (Resolve-Path -LiteralPath $SourceDir).Path
  $srcPrefix = $srcRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar

  $dstRoot = (Resolve-Path -LiteralPath $DestDir).Path
  $dstPrefix = $dstRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar

  function Normalize-RelPathKey {
    param([Parameter(Mandatory = $true)][string] $RelPath)

    # Normalize for case-insensitive comparisons, and normalize separators so we catch collisions
    # across platforms (Windows vs Linux/macOS checkouts).
    $p = $RelPath.Replace("\", "/")
    $p = ($p -replace "/+", "/").Trim("/")
    return $p.ToLowerInvariant()
  }

  # Fail fast on collisions between existing staged content and extra tools.
  # Collisions must be treated case-insensitively so we don't silently clobber files on Windows.
  $existing = @{} # key -> { Path, IsFile }
  $existingEntries = @(Get-ChildItem -LiteralPath $DestDir -Recurse -Force -ErrorAction SilentlyContinue)
  foreach ($e in $existingEntries) {
    if (-not $e.FullName.StartsWith($dstPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
      continue
    }
    $relExisting = $e.FullName.Substring($dstPrefix.Length)
    if ([string]::IsNullOrEmpty($relExisting)) { continue }
    $keyExisting = Normalize-RelPathKey -RelPath $relExisting
    if (-not $existing.ContainsKey($keyExisting)) {
      $existing[$keyExisting] = [pscustomobject]@{
        Path = $e.FullName
        IsFile = -not $e.PSIsContainer
      }
    }
  }

  $copied = New-Object System.Collections.Generic.List[string]

  $files = @(
    Get-ChildItem -LiteralPath $SourceDir -Recurse -Force -File -ErrorAction Stop |
      Sort-Object -Property FullName
  )

  $planned = New-Object System.Collections.Generic.List[object]
  $plannedKeys = @{} # key -> rel
  foreach ($f in $files) {
    $ext = $f.Extension
    $extNoDot = $ext
    if ($extNoDot.StartsWith(".")) {
      $extNoDot = $extNoDot.Substring(1)
    }
    $extNoDotLower = $extNoDot.ToLowerInvariant()
    if (-not [string]::IsNullOrEmpty($extNoDotLower) -and (Test-PrivateKeyExtension -ExtensionNoDotLower $extNoDotLower)) {
      throw "Refusing to stage private key material (.$extNoDotLower) from ExtraToolsDir: $($f.FullName)"
    }

    if (-not $f.FullName.StartsWith($srcPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
      throw "Internal error: expected '$($f.FullName)' to be under '$srcRoot'."
    }
    $rel = $f.FullName.Substring($srcPrefix.Length)

    # Skip hidden files/dirs to keep outputs stable across hosts.
    if (Test-HiddenRelPath -RelPath $rel) {
      continue
    }

    # Ignore common Windows shell metadata files.
    $nameLower = $f.Name.ToLowerInvariant()
    if ($nameLower -in @("thumbs.db", "ehthumbs.db", "desktop.ini")) {
      continue
    }

    # Match `aero_packager`'s default exclusions for the optional `tools/` tree: avoid copying
    # debug symbols, build outputs, and source/project files that will not be packaged anyway.
    if (-not [string]::IsNullOrEmpty($extNoDotLower) -and (Test-DefaultExcludedToolsExtension -ExtensionNoDotLower $extNoDotLower)) {
      continue
    }

    $dst = Join-Path $DestDir $rel
    $key = Normalize-RelPathKey -RelPath $rel

    # Detect directory/file collisions (e.g. existing `tools/bin` file vs new `tools/bin/foo.exe`).
    $parts = @($key -split "/")
    for ($i = 0; $i -lt ($parts.Count - 1); $i++) {
      $parentKey = ($parts[0..$i] -join "/")
      if ($plannedKeys.ContainsKey($parentKey)) {
        throw "ExtraToolsDir contains a directory/file collision: '$rel' requires directory '$parentKey' but a file is already staged from ExtraToolsDir at '$($plannedKeys[$parentKey])'."
      }
      if ($existing.ContainsKey($parentKey) -and $existing[$parentKey].IsFile) {
        throw "ExtraToolsDir staging would collide with an existing staged file: '$rel' requires directory '$parentKey' but '$($existing[$parentKey].Path)' exists as a file."
      }
    }

    if ($plannedKeys.ContainsKey($key)) {
      throw "ExtraToolsDir contains a case-insensitive path collision: '$rel' collides with '$($plannedKeys[$key])'."
    }
    if ($existing.ContainsKey($key)) {
      throw "ExtraToolsDir staging would overwrite an existing staged path (case-insensitive): '$rel' collides with '$($existing[$key].Path)'."
    }

    $plannedKeys[$key] = $rel
    [void]$planned.Add([pscustomobject]@{ Source = $f.FullName; Rel = $rel; Key = $key; Dest = $dst })
  }

  foreach ($p in ($planned | Sort-Object -Property Key)) {
    $dstParent = Split-Path -Parent $p.Dest
    if (-not (Test-Path -LiteralPath $dstParent -PathType Container)) {
      New-Item -ItemType Directory -Force -Path $dstParent | Out-Null
    }

    # Use .NET file copy with overwrite=false as a final guard against accidental overwrite.
    [System.IO.File]::Copy($p.Source, $p.Dest, $false)
    [void]$copied.Add(("tools/" + ($p.Rel.Replace("\", "/"))))
  }

  return ,$copied.ToArray()
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
    # Prefer SOURCE_DATE_EPOCH when available so Guest Tools artifact naming can be reproducible
    # even in environments without a working git checkout.
    $sde = $env:SOURCE_DATE_EPOCH
    if (-not [string]::IsNullOrWhiteSpace($sde)) {
      try {
        $epoch = [int64] $sde.Trim()
        $date = [DateTimeOffset]::FromUnixTimeSeconds($epoch).ToString("yyyyMMdd", [System.Globalization.CultureInfo]::InvariantCulture)
      } catch {
        $date = $null
      }
    }
    if ($null -eq $date) {
      $date = Get-Date -Format "yyyyMMdd"
    }
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
    # Historical layout used nested paths (windows7/virtio/{blk,net}).
    "windows7/virtio/blk" = "virtio-blk"
    "windows7/virtio/net" = "virtio-net"
    # Current canonical layout uses top-level virtio-{blk,net} directories.
    "windows7/virtio-blk" = "virtio-blk"
    "windows7/virtio-net" = "virtio-net"
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

function Read-TextFileWithEncodingDetection {
  param([Parameter(Mandatory = $true)][string] $Path)

  # PowerShell's Get-Content encoding detection relies primarily on BOMs. Some real-world
  # driver INFs ship as UTF-16LE without a BOM, which results in NUL-padded text and breaks
  # AddService/HWID scanning for device-contract auto-patching.
  #
  # We only need best-effort text for pattern matching, so implement lightweight detection:
  #   - UTF-8 BOM
  #   - UTF-16LE/BE BOM
  #   - BOM-less UTF-16 heuristic (even length + high NUL ratio; infer endianness by NUL distribution)
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  if ($null -eq $bytes -or $bytes.Length -eq 0) {
    return ""
  }

  $offset = 0
  $encoding = $null

  if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    $encoding = [System.Text.Encoding]::UTF8
    $offset = 3
  } elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
    $encoding = [System.Text.Encoding]::Unicode # UTF-16LE
    $offset = 2
  } elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) {
    $encoding = [System.Text.Encoding]::BigEndianUnicode # UTF-16BE
    $offset = 2
  } elseif (($bytes.Length % 2) -eq 0 -and $bytes.Length -ge 4) {
    # Heuristic for BOM-less UTF-16. INF files are typically ASCII-ish, so UTF-16 text tends
    # to have a high number of 0x00 bytes in either even or odd positions.
    $pairs = [int]($bytes.Length / 2)
    $nulEven = 0
    $nulOdd = 0
    for ($i = 0; $i -lt $bytes.Length; $i += 2) {
      if ($bytes[$i] -eq 0) { $nulEven += 1 }
    }
    for ($i = 1; $i -lt $bytes.Length; $i += 2) {
      if ($bytes[$i] -eq 0) { $nulOdd += 1 }
    }

    $nulRatio = ($nulEven + $nulOdd) / [double]$bytes.Length
    $evenRatio = $nulEven / [double]$pairs
    $oddRatio = $nulOdd / [double]$pairs

    if ($nulRatio -ge 0.2 -and ([Math]::Max($evenRatio, $oddRatio) -ge 0.5)) {
      # If odd bytes are mostly NULs, that's typical UTF-16LE. If even bytes are mostly NULs,
      # that's typical UTF-16BE.
      $encoding = if ($oddRatio -ge $evenRatio) { [System.Text.Encoding]::Unicode } else { [System.Text.Encoding]::BigEndianUnicode }
      $offset = 0
    }
  }

  if (-not $encoding) {
    $encoding = [System.Text.Encoding]::UTF8
    $offset = 0
  }

  $text = $encoding.GetString($bytes, $offset, ($bytes.Length - $offset))

  # Remove any leading BOM codepoint (defensive), and strip NULs in case decoding fell back
  # to the wrong encoding for an unexpected file.
  if ($text.Length -gt 0 -and $text[0] -eq [char]0xFEFF) {
    $text = $text.Substring(1)
  }
  if ($text.IndexOf([char]0) -ge 0) {
    $text = $text.Replace([char]0, "")
  }

  return $text
}

function Get-InfAddServiceNames {
  param([Parameter(Mandatory = $true)][string] $InfPath)

  $content = $null
  try {
    $content = Read-TextFileWithEncodingDetection -Path $InfPath
  } catch {
    return @()
  }

  $names = @{}
  foreach ($rawLine in ($content -split "`r?`n")) {
    $line = $rawLine
    if ($line.Length -gt 0 -and $line[0] -eq [char]0xFEFF) {
      $line = $line.Substring(1)
    }

    # Strip inline INF comments before parsing AddService so the extracted service name doesn't
    # include a trailing ';' (e.g. `AddService = viostor; comment, ...`).
    $semi = $line.IndexOf(';')
    if ($semi -ge 0) {
      $line = $line.Substring(0, $semi)
    }
    $line = $line.Trim()
    if (-not $line) { continue }

    $m = [regex]::Match($line, "(?i)^\\s*AddService\\s*=\\s*(.+)$")
    if (-not $m.Success) { continue }

    $rest = $m.Groups[1].Value.Trim()
    if ($rest.Length -eq 0) { continue }
    $rest = $rest.Replace('"', '')

    $svc = $null
    $m2 = [regex]::Match($rest, "^([^,\\s]+)")
    if ($m2.Success) {
      $svc = $m2.Groups[1].Value.Trim().TrimEnd(';').Trim()
    }
    if ([string]::IsNullOrWhiteSpace($svc)) { continue }

    $key = $svc.ToLowerInvariant()
    if (-not $names.ContainsKey($key)) {
      $names[$key] = $svc
    }
  }

  return ,($names.Values | Sort-Object)
}

function Test-InfTextContainsAnyHardwareId {
  param(
    [Parameter(Mandatory = $true)][string] $InfText,
    [Parameter(Mandatory = $true)][string[]] $HardwareIds
  )

  # INF files are line-oriented and use `;` for comments. We only want to treat a hardware ID
  # as "present" if it appears in a non-comment portion of the file; otherwise we can pick up
  # stale commented-out HWIDs and patch the device contract based on the wrong driver.
  foreach ($rawLine in ($InfText -split "`r?`n")) {
    $line = $rawLine
    $semi = $line.IndexOf(';')
    if ($semi -ge 0) {
      $line = $line.Substring(0, $semi)
    }
    $line = $line.Trim()
    if (-not $line) { continue }

    foreach ($hwid in $HardwareIds) {
      if (-not $hwid) { continue }
      if ($line.IndexOf($hwid, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
        return $true
      }
    }
  }

  return $false
}

function Update-WindowsDeviceContractDriverServiceNamesFromDrivers {
  param(
    [Parameter(Mandatory = $true)][string] $ContractPath,
    [Parameter(Mandatory = $true)][string] $StageDriversRoot,
    # Device names in the Windows device contract that should have driver_service_name auto-aligned
    # with the staged INF AddService names.
    [string[]] $Devices = @("virtio-blk", "virtio-net", "virtio-input", "virtio-snd")
  )

  if (-not (Test-Path -LiteralPath $ContractPath -PathType Leaf)) {
    throw "Windows device contract JSON not found: $ContractPath"
  }

  $contract = Get-Content -LiteralPath $ContractPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
  if (-not $contract -or -not $contract.devices) {
    throw "Windows device contract JSON is missing the required 'devices' field: $ContractPath"
  }

  $infFiles = @(
    Get-ChildItem -LiteralPath $StageDriversRoot -Recurse -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -match '(?i)\\.inf$' } |
      Sort-Object -Property FullName
  )
  if (-not $infFiles -or $infFiles.Count -eq 0) {
    Write-Warning "No .inf files found under staged drivers root ($StageDriversRoot); skipping driver_service_name auto-detection."
    return
  }

  function Get-ContractDeviceEntry {
    param(
      [Parameter(Mandatory = $true)] $ContractObj,
      [Parameter(Mandatory = $true)][string] $DeviceName
    )

    foreach ($d in @($ContractObj.devices)) {
      if ($d -and $d.device -and ($d.device -ieq $DeviceName)) {
        return $d
      }
    }
    return $null
  }

  $anyChanged = $false

  foreach ($deviceName in $Devices) {
    if ([string]::IsNullOrWhiteSpace($deviceName)) { continue }

    $entry = Get-ContractDeviceEntry -ContractObj $contract -DeviceName $deviceName
    if (-not $entry) {
      # Be permissive: allow custom contracts that omit some virtio devices.
      continue
    }

    $currentService = ("" + $entry.driver_service_name).Trim()
    $hwids = @()
    if ($entry.hardware_id_patterns) {
      foreach ($p in $entry.hardware_id_patterns) {
        $t = ("" + $p).Trim()
        if ($t.Length -gt 0) { $hwids += $t }
      }
    }
    if (-not $hwids -or $hwids.Count -eq 0) {
      continue
    }

    $serviceToInfs = @{}
    foreach ($inf in $infFiles) {
      $text = $null
      try {
        $text = Read-TextFileWithEncodingDetection -Path $inf.FullName
      } catch {
        continue
      }
      if (-not (Test-InfTextContainsAnyHardwareId -InfText $text -HardwareIds $hwids)) { continue }

      foreach ($svc in (Get-InfAddServiceNames -InfPath $inf.FullName)) {
        $k = $svc.ToLowerInvariant()
        if (-not $serviceToInfs.ContainsKey($k)) {
          $serviceToInfs[$k] = New-Object System.Collections.Generic.List[string]
        }
        [void]$serviceToInfs[$k].Add($inf.FullName)
      }
    }

    if ($serviceToInfs.Count -eq 0) {
      continue
    }

    $serviceCandidates = @($serviceToInfs.Keys | Sort-Object)
    $selected = $null

    # Prefer the existing contract value if it's among the candidates.
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
          throw "Unable to determine a unique driver service name for '$deviceName' from staged driver INFs. Candidates: $($serviceCandidates -join ', ').`nINF matches:`n$($details -join \"`n\")"
        }
      }
    }

    if (-not $selected) { continue }
    if (-not [string]::IsNullOrWhiteSpace($currentService) -and ($selected.ToLowerInvariant() -eq $currentService.ToLowerInvariant())) {
      continue
    }

    Write-Host "Patching Windows device contract: $deviceName driver_service_name=$selected"
    $entry.driver_service_name = $selected
    $anyChanged = $true
  }

  if (-not $anyChanged) {
    return
  }

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

# Name collision repro snippets (manual):
#
# Packager layout (`x86/` + `amd64/`):
#
#   $root = Join-Path $env:TEMP ("aero-gt-collision-{0}" -f [Guid]::NewGuid().ToString("N"))
#   New-Item -ItemType Directory -Force -Path (Join-Path $root "x86\\aerogpu") | Out-Null
#   New-Item -ItemType Directory -Force -Path (Join-Path $root "x86\\aero-gpu") | Out-Null
#   New-Item -ItemType Directory -Force -Path (Join-Path $root "amd64\\aerogpu") | Out-Null
#   pwsh -NoProfile -File ci/package-guest-tools.ps1 -InputRoot $root
#
# Bundle layout (`drivers/<driver>/(x86|x64)/...`):
#
#   $root = Join-Path $env:TEMP ("aero-gt-collision-{0}" -f [Guid]::NewGuid().ToString("N"))
#   New-Item -ItemType Directory -Force -Path (Join-Path $root "drivers\\aero-gpu\\x86") | Out-Null
#   New-Item -ItemType Directory -Force -Path (Join-Path $root "drivers\\aerogpu\\x86") | Out-Null
#   pwsh -NoProfile -File ci/package-guest-tools.ps1 -InputRoot $root
#
# Expected: staging fails fast with a "Driver directory name collision" error that names both
# source directories and suggests using -DriverNameMapJson.
#
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

    # Detect normalized-name collisions before copying to avoid silent merges/overwrites
    # (e.g. both `aero-gpu` and `aerogpu` normalizing to `aerogpu`).
    $seenDstNames = @{} # dstName -> source dir full path

    # Deterministic ordering so collision errors are stable in CI logs.
    $driverDirs = @(
      Get-ChildItem -LiteralPath $arch.Src -Directory -ErrorAction SilentlyContinue |
        Sort-Object -Property @{ Expression = { $_.Name.ToLowerInvariant() } }, @{ Expression = { $_.FullName.ToLowerInvariant() } }
    )
    foreach ($d in $driverDirs) {
      $dstName = Normalize-GuestToolsDriverName -Name $d.Name
      $dstKey = $dstName.ToLowerInvariant()
      if ($seenDstNames.ContainsKey($dstKey)) {
        $first = $seenDstNames[$dstKey]
        throw (@(
            "Driver directory name collision while staging from packager layout ($($arch.Out)):",
            "  destination name: '$dstName'",
            "  source #1: '$($first.Name)' ($($first.Path))",
            "  source #2: '$($d.Name)' ($($d.FullName))",
            "",
            "Remediation: remove/rename one of the directories, or pass -DriverNameMapJson to rename one explicitly."
          ) -join "`n")
      }
      $seenDstNames[$dstKey] = [pscustomobject]@{ Name = $d.Name; Path = $d.FullName }

      $dst = Join-Path $destArchDir $dstName
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

  # Detect normalized-name collisions before copying to avoid silent merges/overwrites
  # (e.g. both `aero-gpu` and `aerogpu` normalizing to `aerogpu`).
  $seenByArch = @{
    "x86"   = @{} # dstName -> source dir full path
    "amd64" = @{}
  }

  # Deterministic ordering so collision errors are stable in CI logs.
  $driverDirs = @(
    Get-ChildItem -LiteralPath $driversRoot -Directory -ErrorAction SilentlyContinue |
      Sort-Object -Property @{ Expression = { $_.Name.ToLowerInvariant() } }, @{ Expression = { $_.FullName.ToLowerInvariant() } }
  )
  foreach ($d in $driverDirs) {
    $driverName = Normalize-GuestToolsDriverName -Name $d.Name

    $srcX86 = Join-Path $d.FullName "x86"
    if (Test-Path -LiteralPath $srcX86 -PathType Container) {
      $k = $driverName.ToLowerInvariant()
      if ($seenByArch["x86"].ContainsKey($k)) {
        $first = $seenByArch["x86"][$k]
        throw (@(
            "Driver directory name collision while staging from bundle layout (x86):",
            "  destination name: '$driverName'",
            "  source #1: '$($first.Name)' ($($first.Path))",
            "  source #2: '$($d.Name)' ($($d.FullName))",
            "",
            "Remediation: remove/rename one of the directories, or pass -DriverNameMapJson to rename one explicitly."
          ) -join "`n")
      }
      $seenByArch["x86"][$k] = [pscustomobject]@{ Name = $d.Name; Path = $d.FullName }

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
      $k = $driverName.ToLowerInvariant()
      if ($seenByArch["amd64"].ContainsKey($k)) {
        $first = $seenByArch["amd64"][$k]
        throw (@(
            "Driver directory name collision while staging from bundle layout (amd64):",
            "  destination name: '$driverName'",
            "  source #1: '$($first.Name)' ($($first.Path))",
            "  source #2: '$($d.Name)' ($($d.FullName))",
            "",
            "Remediation: remove/rename one of the directories, or pass -DriverNameMapJson to rename one explicitly."
          ) -join "`n")
      }
      $seenByArch["amd64"][$k] = [pscustomobject]@{ Name = $d.Name; Path = $d.FullName }

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

function Assert-ZipContainsFiles {
  param(
    [Parameter(Mandatory = $true)][string] $ZipPath,
    [Parameter(Mandatory = $true)][string[]] $EntryPaths
  )

  if (-not $EntryPaths -or $EntryPaths.Count -eq 0) {
    return
  }

  Add-Type -AssemblyName System.IO.Compression
  $fs = [System.IO.File]::OpenRead($ZipPath)
  $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
  try {
    $set = @{}
    foreach ($e in $zip.Entries) {
      $set[$e.FullName] = $true
    }
    foreach ($p in $EntryPaths) {
      if (-not $set.ContainsKey($p)) {
        throw "Expected ZIP '$ZipPath' to contain entry '$p'."
      }
    }
  } finally {
    $zip.Dispose()
    $fs.Dispose()
  }
}

function Test-SpecIncludesDriverName {
  param(
    [Parameter(Mandatory = $true)][string] $SpecPath,
    [Parameter(Mandatory = $true)][string] $DriverName
  )

  $raw = Get-Content -LiteralPath $SpecPath -Raw -ErrorAction Stop
  $obj = $raw | ConvertFrom-Json -ErrorAction Stop

  $needle = $DriverName.Trim().ToLowerInvariant()
  $entries = @()
  if ($obj -and $obj.drivers) { $entries += $obj.drivers }
  if ($obj -and $obj.required_drivers) { $entries += $obj.required_drivers }

  foreach ($e in $entries) {
    if (-not $e) { continue }
    $n = ("" + $e.name).Trim()
    if ($n.Length -eq 0) { continue }
    if ($n.ToLowerInvariant() -eq $needle) {
      return $true
    }
  }

  return $false
}

function Assert-GuestToolsZipContainsAeroGpuDbgctl {
  param(
    [Parameter(Mandatory = $true)][string] $ZipPath,
    [Parameter(Mandatory = $true)][string] $SpecPath
  )

  $requiresAerogpu = $false
  try {
    $requiresAerogpu = Test-SpecIncludesDriverName -SpecPath $SpecPath -DriverName "aerogpu"
  } catch {
    # If the spec cannot be parsed here but aero_packager succeeded, treat this as an internal
    # inconsistency and fail loudly.
    throw ("Failed to parse Guest Tools spec JSON at '{0}' for dbgctl packaging check: {1}" -f $SpecPath, $_.Exception.Message)
  }

  if (-not $requiresAerogpu) {
    return
  }

  $expectedEntries = @(
    # `ci/build-aerogpu-dbgctl.ps1` stages dbgctl into:
    #   out/drivers/aerogpu/<arch>/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe
    # `ci/make-catalogs.ps1` then copies build outputs into out/packages/... preserving the `tools/` folder.
    "drivers/amd64/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe",
    "drivers/x86/aerogpu/tools/win7_dbgctl/bin/aerogpu_dbgctl.exe",

    # Tool documentation is shipped alongside the binary.
    "drivers/amd64/aerogpu/tools/win7_dbgctl/README.md",
    "drivers/x86/aerogpu/tools/win7_dbgctl/README.md"
  )

  foreach ($entry in $expectedEntries) {
    try {
      Assert-ZipContainsFile -ZipPath $ZipPath -EntryPath $entry
    } catch {
      throw @"
AeroGPU dbgctl tool (or its documentation) is missing from the packaged Guest Tools ZIP.

Expected:
  - $entry

This usually means dbgctl (and/or its docs) was not staged into the AeroGPU driver package, or the dbgctl build step was skipped.

To fix:
  - Ensure the dbgctl build step runs and stages the binary (see: ci/build-aerogpu-dbgctl.ps1).
  - Ensure drivers/aerogpu/ci-package.json lists the expected output under 'requiredBuildOutputFiles'
    (and/or 'toolFiles' if you are staging a repo-local binary instead of a CI build output).
  - Ensure drivers/aerogpu/ci-package.json includes tools/win7_dbgctl/README.md under 'additionalFiles'
    so the packaged driver folder includes tool documentation.

Guest Tools ZIP:
  $ZipPath
"@
    }
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
          $text = Read-TextFileWithEncodingDetection -Path $inf.FullName
          $lines = @($text -split "`r?`n")
        } catch {
          Write-Host "    (failed to read INF)"
          continue
        }

        $anyMatch = $false
        $printed = 0
        $maxPrint = 50
        for ($i = 0; $i -lt $lines.Count; $i++) {
          $line = $lines[$i]
          # Align with `aero_packager` INF validation: ignore comment-only matches (anything after `;`).
          $lineNoComment = $line
          $semi = $lineNoComment.IndexOf(';')
          if ($semi -ge 0) {
            $lineNoComment = $lineNoComment.Substring(0, $semi)
          }
          $lineNoComment = $lineNoComment.Trim()
          foreach ($entry in $compiled) {
            if ($lineNoComment -and $entry.Regex.IsMatch($lineNoComment)) {
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

function Get-Sha256Hex {
  param([Parameter(Mandatory = $true)][string] $Path)

  $hash = Get-FileHash -Algorithm SHA256 -LiteralPath $Path
  return ([string]$hash.Hash).ToLowerInvariant()
}

function Invoke-GuestToolsPackagingRun {
  param(
    [Parameter(Mandatory = $true)][string] $StageRoot,
    [Parameter(Mandatory = $true)][string] $OutDirResolved,
    [string] $RunLabel = ""
  )

  if (-not [string]::IsNullOrWhiteSpace($RunLabel)) {
    Write-Host ""
    Write-Host ("==== {0} ====" -f $RunLabel)
  }

  $stageDriversRoot = Join-Path $StageRoot "drivers"
  $stageGuestTools = Join-Path $StageRoot "guest-tools"
  $stageInputExtract = Join-Path $StageRoot "input"
  $stageDeviceContract = Join-Path $StageRoot "windows-device-contract.json"

  $success = $false
  try {
    Ensure-EmptyDirectory -Path $StageRoot
    Ensure-EmptyDirectory -Path $stageDriversRoot
    Ensure-EmptyDirectory -Path $stageInputExtract

    Write-Host "Staging Guest Tools..."
    Stage-GuestTools -SourceDir $guestToolsResolved -DestDir $stageGuestTools -CertSourcePath $certPathResolved -IncludeCerts:$includeCerts
    $extraToolsZipEntries = @()
    if ($extraToolsResolved) {
      Write-Host "Staging extra Guest Tools utilities..."
      $stageToolsDir = Join-Path $stageGuestTools "tools"
      if ($extraToolsDirModeNorm -eq "replace") {
        if (Test-Path -LiteralPath $stageToolsDir) {
          Remove-Item -LiteralPath $stageToolsDir -Recurse -Force
        }
      }
      New-Item -ItemType Directory -Force -Path $stageToolsDir | Out-Null
      $extraToolsZipEntries = Copy-TreeWithSafetyFilters -SourceDir $extraToolsResolved -DestDir $stageToolsDir
      if (-not $extraToolsZipEntries -or $extraToolsZipEntries.Count -eq 0) {
        Write-Warning "ExtraToolsDir was provided but no files were staged (all were filtered or the directory is empty)."
      } else {
        Write-Host ("  Staged {0} file(s) under guest-tools/tools/." -f $extraToolsZipEntries.Count)
      }
    }

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
    # Patch a staged copy of the contract so the packaged media uses driver_service_name values
    # that match the staged virtio driver INFs (e.g. viostor/netkvm/... for upstream virtio-win bundles).
    Copy-Item -LiteralPath $windowsDeviceContractResolved -Destination $stageDeviceContract -Force
    Update-WindowsDeviceContractDriverServiceNamesFromDrivers -ContractPath $stageDeviceContract -StageDriversRoot $stageDriversRoot

    New-Item -ItemType Directory -Force -Path $OutDirResolved | Out-Null

    Write-Host "Packaging via aero_packager..."
    Write-Host "  version : $Version"
    Write-Host "  build-id: $BuildId"
    Write-Host "  epoch   : $epoch"
    Write-Host "  policy  : $SigningPolicy"
    Write-Host "  spec    : $specPathResolved"
    Write-Host "  contract: $stageDeviceContract"
    Write-Host "  out     : $OutDirResolved"

    try {
      & cargo run --manifest-path $packagerManifest --release --locked -- `
        --drivers-dir $stageDriversRoot `
        --guest-tools-dir $stageGuestTools `
        --spec $specPathResolved `
        --windows-device-contract $stageDeviceContract `
        --out-dir $OutDirResolved `
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

    $isoPath = Join-Path $OutDirResolved "aero-guest-tools.iso"
    $zipPath = Join-Path $OutDirResolved "aero-guest-tools.zip"
    $manifestPath = Join-Path $OutDirResolved "manifest.json"
    $manifestCopyPath = Join-Path $OutDirResolved "aero-guest-tools.manifest.json"

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
    Assert-GuestToolsZipContainsAeroGpuDbgctl -ZipPath $zipPath -SpecPath $specPathResolved
    if ($extraToolsZipEntries -and $extraToolsZipEntries.Count -gt 0) {
      Assert-ZipContainsFiles -ZipPath $zipPath -EntryPaths $extraToolsZipEntries
    }

    $success = $true

    return [pscustomobject]@{
      OutDirResolved = $OutDirResolved
      IsoPath = $isoPath
      ZipPath = $zipPath
      ManifestPath = $manifestPath
    }
  } finally {
    if ($success -and (Test-Path -LiteralPath $StageRoot)) {
      Remove-Item -LiteralPath $StageRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
}

function Assert-GuestToolsArtifactsMatch {
  param(
    [Parameter(Mandatory = $true)][string] $OutDirA,
    [Parameter(Mandatory = $true)][string] $OutDirB
  )

  $targets = @(
    "aero-guest-tools.iso",
    "aero-guest-tools.zip",
    "manifest.json"
  )

  foreach ($name in $targets) {
    $pathA = Join-Path $OutDirA $name
    $pathB = Join-Path $OutDirB $name

    Assert-FileExistsNonEmpty -Path $pathA
    Assert-FileExistsNonEmpty -Path $pathB

    $hashA = Get-Sha256Hex -Path $pathA
    $hashB = Get-Sha256Hex -Path $pathB
    if ($hashA -ne $hashB) {
      throw ("Determinism self-test failed: '{0}' differs between runs.`n  run1: {1}`n    sha256: {2}`n  run2: {3}`n    sha256: {4}`n`nTemporary output directories have been left on disk for inspection:`n  {5}`n  {6}" -f $name, $pathA, $hashA, $pathB, $hashB, $OutDirA, $OutDirB)
    }
  }
}

$inputRootResolved = Resolve-RepoPath -Path $InputRoot
$guestToolsResolved = Resolve-RepoPath -Path $GuestToolsDir
$extraToolsResolved = $null
$extraToolsDirModeNorm = $ExtraToolsDirMode
if (-not [string]::IsNullOrWhiteSpace($ExtraToolsDir)) {
  $extraToolsResolved = Resolve-RepoPath -Path $ExtraToolsDir
  $extraToolsDirModeNorm = $ExtraToolsDirMode.Trim().ToLowerInvariant()
}
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
if ($extraToolsResolved) {
  if (-not (Test-Path -LiteralPath $extraToolsResolved -PathType Container)) {
    throw "ExtraToolsDir does not exist: '$extraToolsResolved'."
  }
  if ($extraToolsDirModeNorm -ne "merge" -and $extraToolsDirModeNorm -ne "replace") {
    throw "Invalid ExtraToolsDirMode: '$ExtraToolsDirMode'. Expected merge or replace."
  }
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

$windowsDeviceContractResolved = $null
if ([string]::IsNullOrWhiteSpace($WindowsDeviceContractPath)) {
  throw "-WindowsDeviceContractPath must not be empty."
}
$windowsDeviceContractResolved = Resolve-RepoPath -Path $WindowsDeviceContractPath
if (-not (Test-Path -LiteralPath $windowsDeviceContractResolved -PathType Leaf)) {
  throw "WindowsDeviceContractPath does not exist: '$windowsDeviceContractResolved'."
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

$stageRootBase = Resolve-RepoPath -Path "out/_staging_guest_tools"

if ($DeterminismSelfTest) {
  $nonce = ([System.Guid]::NewGuid().ToString("N")).Substring(0, 10)

  $stageRootRun1 = $stageRootBase.TrimEnd("\", "/") + ".determinism-selftest-$nonce-run1"
  $stageRootRun2 = $stageRootBase.TrimEnd("\", "/") + ".determinism-selftest-$nonce-run2"
  $outDirRun1 = $outDirResolved.TrimEnd("\", "/") + ".determinism-selftest-$nonce-run1"
  $outDirRun2 = $outDirResolved.TrimEnd("\", "/") + ".determinism-selftest-$nonce-run2"

  $cleanup = $false
  try {
    Ensure-EmptyDirectory -Path $outDirRun1
    Ensure-EmptyDirectory -Path $outDirRun2

    Invoke-GuestToolsPackagingRun -StageRoot $stageRootRun1 -OutDirResolved $outDirRun1 -RunLabel "Determinism self-test (run 1/2)" | Out-Null
    Invoke-GuestToolsPackagingRun -StageRoot $stageRootRun2 -OutDirResolved $outDirRun2 -RunLabel "Determinism self-test (run 2/2)" | Out-Null

    Assert-GuestToolsArtifactsMatch -OutDirA $outDirRun1 -OutDirB $outDirRun2
    $cleanup = $true

    Write-Host ""
    Write-Host "Determinism self-test passed."
  } finally {
    if ($cleanup) {
      foreach ($p in @($outDirRun1, $outDirRun2, $stageRootRun1, $stageRootRun2)) {
        if (Test-Path -LiteralPath $p) {
          Remove-Item -LiteralPath $p -Recurse -Force -ErrorAction SilentlyContinue
        }
      }
    }
  }

  return
}

Invoke-GuestToolsPackagingRun -StageRoot $stageRootBase -OutDirResolved $outDirResolved | Out-Null

Write-Host "Guest Tools artifacts created in '$outDirResolved':"
Write-Host "  aero-guest-tools.iso"
Write-Host "  aero-guest-tools.zip"
Write-Host "  manifest.json"
Write-Host "  aero-guest-tools.manifest.json"
