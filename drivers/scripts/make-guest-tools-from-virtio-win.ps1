<#
.SYNOPSIS
Build Aero Guest Tools media (ISO + zip) from an upstream virtio-win ISO/root.

.DESCRIPTION
This script wraps:

- `drivers/scripts/make-driver-pack.ps1` (extracts Win7 driver packages from virtio-win)
- `tools/packaging/aero_packager/` (packages Guest Tools scripts + drivers into ISO/zip)

On Windows, `-VirtioWinIso` mounts the ISO via `Mount-DiskImage` (and falls back to
`tools/virtio-win/extract.py` when mounting is unavailable or fails).

On Linux/macOS, run under PowerShell 7 (`pwsh`) and either:

- pass `-VirtioWinIso` (the underlying driver pack script will automatically fall back to
  `tools/virtio-win/extract.py` when `Mount-DiskImage` is unavailable or fails), or
- extract first with `python3 tools/virtio-win/extract.py` and then pass `-VirtioWinRoot`.

Use `-Profile` to choose a predictable driver set:

- `full` (default): includes optional audio/input drivers when present (best-effort)
- `minimal`: storage+network only (aligned with the "minimal" packaging spec)

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
  # - full: includes optional virtio audio/input drivers if present (default)
  # - minimal: storage+network only
  [ValidateSet("minimal", "full")]
  [string]$Profile = "full",

  # Optional: override which driver packages are extracted from virtio-win.
  [string[]]$Drivers,

  # Optional: fail if audio/input drivers are requested but missing from virtio-win.
  [switch]$StrictOptional,

  [string]$SpecPath,

  # Windows device contract used by the Guest Tools packager to generate `config/devices.cmd`.
  #
  # Virtio-win driver bundles must use the virtio-win contract so:
  # - AERO_VIRTIO_*_SERVICE values match the upstream INF AddService names (viostor/netkvm/...)
  # - guest-tools/setup.cmd can pre-seed boot-critical storage registry keys without /skipstorage.
  [string]$WindowsDeviceContractPath = "docs/windows-device-contract-virtio-win.json",

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

if (-not $WindowsDeviceContractPath) {
  throw "-WindowsDeviceContractPath must not be empty."
}
if (-not [System.IO.Path]::IsPathRooted($WindowsDeviceContractPath)) {
  $WindowsDeviceContractPath = Join-Path $repoRoot $WindowsDeviceContractPath
}
$WindowsDeviceContractPath = [System.IO.Path]::GetFullPath($WindowsDeviceContractPath)

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
Write-Host "  contract: $WindowsDeviceContractPath"
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
if (-not (Test-Path -LiteralPath $WindowsDeviceContractPath -PathType Leaf)) {
  throw "Expected Windows device contract not found: $WindowsDeviceContractPath"
}

Ensure-Directory -Path $driversOutDir
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
Copy-Item -Path (Join-Path $guestToolsDir "*") -Destination $guestToolsStageDir -Recurse -Force

$driverPackLicenses = Join-Path $driverPackRoot "licenses"
$stageLicenses = Join-Path $guestToolsStageDir "licenses"
if (Test-Path -LiteralPath $driverPackLicenses -PathType Container) {
  # Merge upstream license/notice texts into the staged Guest Tools tree without discarding any
  # existing repo-provided license files.
  Ensure-Directory -Path $stageLicenses
  Copy-Item -Path (Join-Path $driverPackLicenses "*") -Destination $stageLicenses -Recurse -Force
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
# Use the same staging logic as CI packaging so outputs are deterministic via
# SOURCE_DATE_EPOCH / --source-date-epoch (wrapper default).
#
# Note: `guest-tools/config/devices.cmd` is generated by aero_packager from a Windows
# device contract JSON. For virtio-win packaging, we generate a temporary contract
# override that patches the virtio driver service names to match the extracted
# virtio-win INFs (viostor/netkvm/viosnd/vioinput) so setup.cmd boot-critical storage
# pre-seeding stays aligned with the packaged drivers.
$wrapperScript = Join-Path (Join-Path $repoRoot "ci") "package-guest-tools.ps1"
if (-not (Test-Path -LiteralPath $wrapperScript -PathType Leaf)) {
  throw "Expected Guest Tools packaging wrapper script not found: $wrapperScript"
}

Ensure-Directory -Path $OutDir

Write-Host "Packaging Guest Tools via CI wrapper..."
Write-Host "  spec : $SpecPath"
Write-Host "  out  : $OutDir"
Write-Host "  contract (template) : $WindowsDeviceContractPath"

function Read-TextFileWithEncodingDetection {
  param([Parameter(Mandatory = $true)][string]$Path)

  # PowerShell's Get-Content encoding detection relies primarily on BOMs. Some real-world
  # driver INFs ship as UTF-16LE without a BOM, which results in NUL-padded text and breaks
  # AddService scanning when generating the virtio-win device contract override.
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
    # No BOM and doesn't look like UTF-16. Treat as UTF-8 (ASCII-compatible).
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
  param([Parameter(Mandatory = $true)][string]$InfPath)

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
    if ([string]::IsNullOrWhiteSpace($rest)) { continue }
    $rest = $rest.Replace('"', '')

    $svc = $null
    $m2 = [regex]::Match($rest, "^([^,\\s]+)")
    if ($m2.Success) {
      $svc = $m2.Groups[1].Value.Trim().TrimEnd(';').Trim()
    }
    if ([string]::IsNullOrWhiteSpace($svc)) { continue }

    $key = $svc.ToLowerInvariant()
    if (-not $names.ContainsKey($key)) { $names[$key] = $svc }
  }

  return ,($names.Values | Sort-Object)
}

function Resolve-DriverServiceNameFromInfs {
  param([Parameter(Mandatory = $true)][string]$DriverDir)

  if (-not (Test-Path -LiteralPath $DriverDir -PathType Container)) {
    return $null
  }

  # Note: on Linux, `-Filter "*.inf"` is case-sensitive. Use a case-insensitive match so
  # we don't miss `*.INF` files coming from Windows build artifacts.
  $infFiles = @(
    Get-ChildItem -LiteralPath $DriverDir -Recurse -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -match '(?i)\.inf$' }
  )
  if (-not $infFiles -or $infFiles.Count -eq 0) {
    return $null
  }

  $services = @{}
  foreach ($inf in $infFiles) {
    foreach ($svc in (Get-InfAddServiceNames -InfPath $inf.FullName)) {
      $services[$svc.ToLowerInvariant()] = $svc
    }
  }

  $candidates = @($services.Values | Sort-Object)
  if (-not $candidates -or $candidates.Count -eq 0) { return $null }
  if ($candidates.Count -eq 1) { return $candidates[0] }

  # Disambiguate using the presence of <service>.sys in the driver tree.
  $matching = @()
  foreach ($svc in $candidates) {
    $sysName = "$svc.sys"
    $hit = Get-ChildItem -LiteralPath $DriverDir -Recurse -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -ieq $sysName } |
      Select-Object -First 1
    if ($hit) { $matching += $svc }
  }
  if ($matching.Count -eq 1) { return $matching[0] }

  throw "Unable to determine a unique service name from INF AddService lines under '$DriverDir'. Candidates: $($candidates -join ', ')."
}

function Resolve-PackagedDriverServiceName {
  param(
    [Parameter(Mandatory = $true)][string]$DriversRoot,
    [Parameter(Mandatory = $true)][string]$DriverName
  )

  $x86Dir = Join-Path (Join-Path $DriversRoot "x86") $DriverName
  $amd64Dir = Join-Path (Join-Path $DriversRoot "amd64") $DriverName

  $x86Svc = Resolve-DriverServiceNameFromInfs -DriverDir $x86Dir
  $amd64Svc = Resolve-DriverServiceNameFromInfs -DriverDir $amd64Dir

  if (-not $x86Svc -and -not $amd64Svc) { return $null }
  if ($x86Svc -and $amd64Svc -and ($x86Svc.ToLowerInvariant() -ne $amd64Svc.ToLowerInvariant())) {
    throw "Driver '$DriverName' has mismatched AddService names between x86 ('$x86Svc') and amd64 ('$amd64Svc')."
  }

  if ($x86Svc) { return $x86Svc }
  return $amd64Svc
}

$contractObj = Get-Content -LiteralPath $WindowsDeviceContractPath -Raw | ConvertFrom-Json

function Set-ContractDriverServiceName {
  param(
    [Parameter(Mandatory = $true)]$Contract,
    [Parameter(Mandatory = $true)][string]$DeviceName,
    [Parameter(Mandatory = $true)][string]$ServiceName
  )

  foreach ($d in @($Contract.devices)) {
    if ($null -eq $d) { continue }
    $n = "" + $d.device
    if ($n -and ($n.ToLowerInvariant() -eq $DeviceName.ToLowerInvariant())) {
      $d.driver_service_name = $ServiceName
      return
    }
  }
  throw "Device contract is missing required device entry: $DeviceName"
}

$blkSvc = Resolve-PackagedDriverServiceName -DriversRoot $packagerDriversRoot -DriverName "viostor"
if (-not $blkSvc) { throw "Unable to determine virtio-blk service name from packaged driver INFs (viostor)." }
Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-blk" -ServiceName $blkSvc

$netSvc = Resolve-PackagedDriverServiceName -DriversRoot $packagerDriversRoot -DriverName "netkvm"
if (-not $netSvc) { throw "Unable to determine virtio-net service name from packaged driver INFs (netkvm)." }
Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-net" -ServiceName $netSvc

$sndSvc = Resolve-PackagedDriverServiceName -DriversRoot $packagerDriversRoot -DriverName "viosnd"
if ($sndSvc) { Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-snd" -ServiceName $sndSvc }

$inputSvc = Resolve-PackagedDriverServiceName -DriversRoot $packagerDriversRoot -DriverName "vioinput"
if ($inputSvc) { Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-input" -ServiceName $inputSvc }

if ($contractObj.contract_version) {
  $contractObj.contract_version = ("" + $contractObj.contract_version) + "+virtio-win"
}

$contractOverridePath = Join-Path $guestToolsStageDir "windows-device-contract.virtio-win.json"
$contractJson = $contractObj | ConvertTo-Json -Depth 50
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($contractOverridePath, ($contractJson + "`n"), $utf8NoBom)

Write-Host "  contract (override): $contractOverridePath"

& $wrapperScript `
  -InputRoot $packagerDriversRoot `
  -GuestToolsDir $guestToolsStageDir `
  -SigningPolicy $SigningPolicy `
  -CertPath $CertPath `
  -WindowsDeviceContractPath $contractOverridePath `
  -SpecPath $SpecPath `
  -OutDir $OutDir `
  -Version $Version `
  -BuildId $BuildId
Write-Host "Done."

if ($CleanStage) {
  Write-Host "Cleaning staging directories..."
  Remove-Item -LiteralPath $driverPackRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $packagerDriversRoot -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $guestToolsStageDir -Recurse -Force -ErrorAction SilentlyContinue
}

