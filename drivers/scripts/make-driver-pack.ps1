<#
.SYNOPSIS
Build an Aero Windows 7 driver pack from an upstream virtio-win ISO (or extracted root).

.DESCRIPTION
On Windows, this script can mount `virtio-win.iso` directly via `Mount-DiskImage` by
passing `-VirtioWinIso`.

On Linux/macOS, `Mount-DiskImage` is not available. Use the cross-platform extractor
(`python3 tools/virtio-win/extract.py --virtio-win-iso … --out-root …`) and then pass
`-VirtioWinRoot` to the extracted directory.

Alternatively, you may pass `-VirtioWinIso` when running under PowerShell 7 (`pwsh`)
on Linux/macOS: this script will automatically fall back to running the extractor if
`Mount-DiskImage` is unavailable (requires Python + `7z` or `pycdlib`).

The produced driver pack staging directory/zip includes:

- `manifest.json` (source provenance: virtio-win ISO hash/volume label/version hints)
- `THIRD_PARTY_NOTICES.md` (redistribution attribution template)
- `licenses/virtio-win/` (best-effort copy of upstream virtio-win LICENSE/NOTICE files when present)

.EXAMPLE
# Windows:
powershell -ExecutionPolicy Bypass -File .\drivers\scripts\make-driver-pack.ps1 `
  -VirtioWinIso C:\path\to\virtio-win.iso

.EXAMPLE
# Linux/macOS:
python3 tools/virtio-win/extract.py --virtio-win-iso virtio-win.iso --out-root /tmp/virtio-win-root
pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot /tmp/virtio-win-root
#>

[CmdletBinding(DefaultParameterSetName = "FromRoot")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutDir = (Join-Path (Join-Path $PSScriptRoot "..") "out"),

  [string[]]$OsFolderCandidates = @("w7", "w7.1", "win7"),

  [string[]]$ArchCandidatesAmd64 = @("amd64", "x64"),
  [string[]]$ArchCandidatesX86 = @("x86", "i386"),

  # Which virtio-win driver packages to extract.
  #
  # Win7 audio/input support varies by virtio-win version; by default this script
  # requires storage+network (viostor, netkvm) and attempts to include audio/input
  # (viosnd, vioinput) on a best-effort basis.
  [string[]]$Drivers = @("viostor", "netkvm", "viosnd", "vioinput"),

  # If set, fail when optional drivers are requested but missing.
  [switch]$StrictOptional,

  [switch]$NoZip
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Derive-VirtioWinVersion {
  param(
    [string]$IsoPath,
    [string]$VirtioRoot
  )

  # Best-effort: some virtio-win ISOs include a version marker at the root.
  foreach ($candidate in @("VERSION", "VERSION.txt", "version.txt", "virtio-win-version.txt")) {
    if (-not $VirtioRoot) { break }
    $p = Join-Path $VirtioRoot $candidate
    if (Test-Path -LiteralPath $p -PathType Leaf) {
      try {
        $line = (Get-Content -LiteralPath $p -TotalCount 1 -ErrorAction Stop).Trim()
        if ($line) { return $line }
      } catch {
        # Ignore and keep trying other heuristics.
      }
    }
  }

  if ($IsoPath) {
    $base = [System.IO.Path]::GetFileNameWithoutExtension($IsoPath)
    if ($base -match '(?i)^virtio-win-(.+)$') {
      return $Matches[1]
    }
  }

  return $null
}

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path (Join-Path $PSScriptRoot "..") "..")).Path
}

function Resolve-Python {
  $candidates = @("python3", "python", "py")
  foreach ($c in $candidates) {
    $cmd = Get-Command $c -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
  }
  return $null
}

function Read-VirtioWinProvenance {
  param(
    [Parameter(Mandatory = $true)]
    [string]$VirtioRoot
  )

  $p = Join-Path $VirtioRoot "virtio-win-provenance.json"
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    return $null
  }
  try {
    return Get-Content -LiteralPath $p -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
  } catch {
    Write-Warning "Failed to parse virtio-win provenance file: $p ($($_.Exception.Message))"
    return $null
  }
}

function Copy-VirtioWinNotices {
  param(
    [Parameter(Mandatory = $true)]
    [string]$VirtioRoot,
    [Parameter(Mandatory = $true)]
    [string]$DestDir
  )

  # Best-effort: copy upstream license/notice files from the virtio-win distribution root.
  #
  # virtio-win ISO contents vary across releases; keep this conservative by only
  # copying common top-level notice files when present.
  $patterns = @(
    "LICENSE*",
    "COPYING*",
    "NOTICE*",
    "EULA*",
    "AUTHORS*",
    "CREDITS*",
    "README*"
  )

  $copied = New-Object "System.Collections.Generic.List[string]"
  $seen = New-Object "System.Collections.Generic.HashSet[string]"

  $rootFiles = @()
  try {
    $rootFiles = Get-ChildItem -LiteralPath $VirtioRoot -File -ErrorAction Stop
  } catch {
    return @()
  }

  foreach ($pat in $patterns) {
    foreach ($h in ($rootFiles | Where-Object { $_.Name -like $pat })) {
      $key = $h.Name.ToLowerInvariant()
      if (-not $seen.Add($key)) {
        continue
      }
      if (-not (Test-Path -LiteralPath $DestDir -PathType Container)) {
        New-Item -ItemType Directory -Path $DestDir -Force | Out-Null
      }
      Copy-Item -LiteralPath $h.FullName -Destination (Join-Path $DestDir $h.Name) -Force
      $copied.Add($h.Name) | Out-Null
    }
  }

  return @($copied | Sort-Object)
}

function Find-ChildDir {
  param(
    [Parameter(Mandatory = $true)]
    [string]$BaseDir,
    [Parameter(Mandatory = $true)]
    [string[]]$Names
  )

  $children = Get-ChildItem -Path $BaseDir -Directory
  foreach ($name in $Names) {
    $hit = $children | Where-Object { $_.Name -ieq $name } | Select-Object -First 1
    if ($null -ne $hit) {
      return $hit.FullName
    }
  }
  return $null
}

function Copy-VirtioWinDriver {
  param(
    [Parameter(Mandatory = $true)]
    [string]$VirtioRoot,
    [Parameter(Mandatory = $true)]
    [string]$DriverDirName,
    [Parameter(Mandatory = $true)]
    [string[]]$OsDirCandidates,
    [Parameter(Mandatory = $true)]
    [string[]]$ArchCandidates,
    [Parameter(Mandatory = $true)]
    [string]$DestDir
  )

  $driverBase = Find-ChildDir -BaseDir $VirtioRoot -Names @($DriverDirName)
  if ($null -eq $driverBase) {
    throw "Could not find driver directory '$DriverDirName' under '$VirtioRoot'."
  }

  $osBase = Find-ChildDir -BaseDir $driverBase -Names $OsDirCandidates
  if ($null -eq $osBase) {
    throw "Could not find an OS directory under '$driverBase'. Tried: $($OsDirCandidates -join ', ')"
  }

  $archBase = Find-ChildDir -BaseDir $osBase -Names $ArchCandidates
  if ($null -eq $archBase) {
    throw "Could not find arch directory under '$osBase'. Tried: $($ArchCandidates -join ', ')"
  }

  New-Item -ItemType Directory -Path $DestDir -Force | Out-Null
  Copy-Item -Path (Join-Path $archBase "*") -Destination $DestDir -Recurse -Force
}

$driverDefs = @{
  "viostor"  = @{ Name = "viostor"; Upstream = "viostor"; Required = $true }
  "netkvm"   = @{ Name = "netkvm"; Upstream = "NetKVM"; Required = $true }
  "viosnd"   = @{ Name = "viosnd"; Upstream = "viosnd"; Required = $false }
  "vioinput" = @{ Name = "vioinput"; Upstream = "vioinput"; Required = $false }
}

$requiredDrivers = @($driverDefs.Values | Where-Object { $_.Required } | ForEach-Object { $_.Name })

$requestedDrivers = New-Object "System.Collections.Generic.List[string]"
$seenDrivers = New-Object "System.Collections.Generic.HashSet[string]"
foreach ($d in $Drivers) {
  if ($null -eq $d) {
    continue
  }
  $id = $d.Trim().ToLowerInvariant()
  if ($id.Length -eq 0) {
    continue
  }
  if ($seenDrivers.Add($id)) {
    $requestedDrivers.Add($id) | Out-Null
  }
}

if ($requestedDrivers.Count -eq 0) {
  throw "-Drivers must include at least one driver. Supported: $($driverDefs.Keys -join ', ')"
}

$unknown = @()
foreach ($id in $requestedDrivers) {
  if (-not $driverDefs.ContainsKey($id)) {
    $unknown += $id
  }
}
if ($unknown.Count -gt 0) {
  throw "Unknown driver(s) requested: $($unknown -join ', '). Supported: $($driverDefs.Keys -join ', ')"
}

$missingRequiredInRequest = @()
foreach ($req in $requiredDrivers) {
  if ($requestedDrivers -notcontains $req) {
    $missingRequiredInRequest += $req
  }
}
if ($missingRequiredInRequest.Count -gt 0) {
  throw "This driver pack requires: $($requiredDrivers -join ', '). Missing from -Drivers: $($missingRequiredInRequest -join ', ')"
}

$mounted = $false
$isoPath = $null
$isoHash = $null
$isoVolumeLabel = $null
$extractTempDir = $null

try {
  if ($PSCmdlet.ParameterSetName -eq "FromIso") {
    $isoPath = (Resolve-Path $VirtioWinIso).Path
    $isoHash = (Get-FileHash -Algorithm SHA256 -Path $isoPath).Hash.ToLowerInvariant()
    $mountCmd = Get-Command "Mount-DiskImage" -ErrorAction SilentlyContinue
    if ($mountCmd) {
      try {
        $img = Mount-DiskImage -ImagePath $isoPath -PassThru
        $mounted = $true
        # Drive letter assignment can be asynchronous on some hosts; poll briefly before failing.
        $vol = $null
        for ($i = 0; $i -lt 20; $i++) {
          $vols = $img | Get-Volume -ErrorAction SilentlyContinue
          $vol = $vols | Where-Object { $_.DriveLetter } | Select-Object -First 1
          if ($vol) { break }
          Start-Sleep -Milliseconds 200
        }
        if (-not $vol -or -not $vol.DriveLetter) {
          throw "Mounted virtio-win ISO has no drive letter assigned: $isoPath"
        }
        $isoVolumeLabel = $vol.FileSystemLabel
        $VirtioWinRoot = "$($vol.DriveLetter):\"
      } catch {
        # Some hosts (or restricted Windows environments) may expose Mount-DiskImage but
        # fail to mount. Fall back to the cross-platform extractor when possible.
        Write-Warning "Mount-DiskImage failed ($($_.Exception.Message)); falling back to the virtio-win extractor."
        if ($mounted -and $null -ne $isoPath) {
          Dismount-DiskImage -ImagePath $isoPath | Out-Null
          $mounted = $false
        }
        $mountCmd = $null
      }
    }
    if (-not $mountCmd) {
      # Non-Windows hosts (and some minimal/restricted Windows installs) don't have Mount-DiskImage
      # available (or it may fail to mount). Fall back to the cross-platform extractor.
      $python = Resolve-Python
      if (-not $python) {
        throw "Mount-DiskImage is unavailable or failed, and Python was not found on PATH. Install Python 3 and re-run, or extract manually and use -VirtioWinRoot."
      }
      $repoRoot = Resolve-RepoRoot
      $extractor = Join-Path (Join-Path (Join-Path $repoRoot "tools") "virtio-win") "extract.py"
      if (-not (Test-Path -LiteralPath $extractor -PathType Leaf)) {
        throw "virtio-win extractor not found: $extractor"
      }

      $extractTempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("aero-virtio-win-" + [System.Guid]::NewGuid().ToString("n"))
      New-Item -ItemType Directory -Path $extractTempDir -Force | Out-Null
      $VirtioWinRoot = Join-Path $extractTempDir "virtio-win-root"

      Write-Host "Extracting virtio-win ISO using $extractor..."
      & $python $extractor --virtio-win-iso $isoPath --out-root $VirtioWinRoot
      if ($LASTEXITCODE -ne 0) {
        throw "virtio-win extractor failed (exit $LASTEXITCODE)."
      }

      # Ingest provenance (volume label, etc) from the extracted root.
      $prov = Read-VirtioWinProvenance -VirtioRoot $VirtioWinRoot
      if ($prov -and $prov.virtio_win_iso -and -not $isoVolumeLabel -and $prov.virtio_win_iso.volume_id) {
        $isoVolumeLabel = ("" + $prov.virtio_win_iso.volume_id).Trim()
      }
    }
  } else {
    $VirtioWinRoot = (Resolve-Path $VirtioWinRoot).Path

    # If the directory came from `tools/virtio-win/extract.py`, reuse its recorded ISO
    # provenance (so non-Windows builds still record the original ISO hash).
    $prov = Read-VirtioWinProvenance -VirtioRoot $VirtioWinRoot
    if ($prov -and $prov.virtio_win_iso) {
      if (-not $isoPath -and $prov.virtio_win_iso.path) {
        $isoPath = "" + $prov.virtio_win_iso.path
      }
      if (-not $isoHash -and $prov.virtio_win_iso.sha256) {
        $h = ("" + $prov.virtio_win_iso.sha256).Trim()
        if ($h -match '^[0-9a-fA-F]{64}$') {
          $isoHash = $h.ToLowerInvariant()
        } else {
          Write-Warning "virtio-win provenance file contains an invalid sha256; ignoring."
        }
      }
      if (-not $isoVolumeLabel -and $prov.virtio_win_iso.volume_id) {
        $isoVolumeLabel = ("" + $prov.virtio_win_iso.volume_id).Trim()
      }
    }
  }

  if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
  }
  $out = (Resolve-Path $OutDir).Path

  $packRoot = Join-Path $out "aero-win7-driver-pack"
  if (Test-Path $packRoot) {
    Remove-Item -Path $packRoot -Recurse -Force
  }

  $win7Root = Join-Path $packRoot "win7"
  $win7Amd64 = Join-Path $win7Root "amd64"
  $win7X86 = Join-Path $win7Root "x86"

  New-Item -ItemType Directory -Path $win7Amd64 -Force | Out-Null
  New-Item -ItemType Directory -Path $win7X86 -Force | Out-Null

  Copy-Item -Path (Join-Path $PSScriptRoot "install.cmd") -Destination (Join-Path $packRoot "install.cmd") -Force
  Copy-Item -Path (Join-Path $PSScriptRoot "enable-testsigning.cmd") -Destination (Join-Path $packRoot "enable-testsigning.cmd") -Force

  $noticesSrc = Join-Path (Join-Path (Join-Path $PSScriptRoot "..") "virtio") "THIRD_PARTY_NOTICES.md"
  if (-not (Test-Path -LiteralPath $noticesSrc -PathType Leaf)) {
    throw "Expected third-party notices file not found: $noticesSrc"
  }
  Copy-Item -LiteralPath $noticesSrc -Destination (Join-Path $packRoot "THIRD_PARTY_NOTICES.md") -Force

  $virtioReadmeSrc = Join-Path (Join-Path (Join-Path $PSScriptRoot "..") "virtio") "README.md"
  if (Test-Path -LiteralPath $virtioReadmeSrc -PathType Leaf) {
    Copy-Item -LiteralPath $virtioReadmeSrc -Destination (Join-Path $packRoot "README.md") -Force
  }

  $warnings = New-Object "System.Collections.Generic.List[string]"

  # Upstream virtio-win license/notice texts (best-effort). Stored under
  # licenses/virtio-win/ to avoid colliding with Aero's own README files.
  $virtioWinNoticeFiles = Copy-VirtioWinNotices -VirtioRoot $VirtioWinRoot -DestDir (Join-Path (Join-Path $packRoot "licenses") "virtio-win")
  if ($virtioWinNoticeFiles.Count -eq 0) {
    $msg = "No upstream virtio-win license/notice files were found at the virtio-win root ('$VirtioWinRoot'). " +
      "The pack will include THIRD_PARTY_NOTICES.md, but you must ensure the correct upstream license texts are included for redistribution."
    $warnings.Add($msg) | Out-Null
    Write-Warning $msg
  }

  $driverResults = @()
  $includedDrivers = New-Object "System.Collections.Generic.HashSet[string]"
  $optionalMissing = New-Object "System.Collections.Generic.List[object]"

  foreach ($name in $requestedDrivers) {
    $drv = $driverDefs[$name]
    $up = $drv.Upstream
    $isRequired = [bool]$drv.Required

    Write-Host "Packing $name (from $up)..."

    $targets = @(
      @{ Id = "win7-amd64"; ArchCandidates = $ArchCandidatesAmd64; DestDir = (Join-Path $win7Amd64 $name) },
      @{ Id = "win7-x86"; ArchCandidates = $ArchCandidatesX86; DestDir = (Join-Path $win7X86 $name) }
    )

    $includedTargets = @()
    $missingTargets = @()
    $targetErrors = @{}

    foreach ($t in $targets) {
      try {
        Copy-VirtioWinDriver -VirtioRoot $VirtioWinRoot -DriverDirName $up -OsDirCandidates $OsFolderCandidates -ArchCandidates $t.ArchCandidates -DestDir $t.DestDir
        $includedTargets += $t.Id
      } catch {
        $msg = $_.Exception.Message
        $missingTargets += $t.Id
        $targetErrors[$t.Id] = $msg
      }
    }

    $status = "included"
    if ($includedTargets.Count -eq 0) {
      $status = "missing"
    } elseif ($missingTargets.Count -gt 0) {
      $status = "partial"
    }

    if ($includedTargets.Count -gt 0) {
      $includedDrivers.Add($name) | Out-Null
    }

    $driverResults += @{
      name = $name
      upstream = $up
      required = $isRequired
      status = $status
      included_targets = $includedTargets
      missing_targets = $missingTargets
      errors = $targetErrors
    }

    if ($missingTargets.Count -gt 0) {
      $summary = "Driver '$name' ($up) missing for: $($missingTargets -join ', ')."
      if ($isRequired) {
        $detail = ""
        if ($targetErrors.Count -gt 0) {
          $detail = " Details: " + (($targetErrors.Keys | Sort-Object) | ForEach-Object { "$_=$($targetErrors[$_])" } -join " | ")
        }
        throw "$summary$detail"
      }

      $detail = ""
      if ($targetErrors.Count -gt 0) {
        $detail = " Details: " + (($targetErrors.Keys | Sort-Object) | ForEach-Object { "$_=$($targetErrors[$_])" } -join " | ")
      }
      $msg = "$summary$detail"
      $warnings.Add($msg) | Out-Null
      Write-Warning $msg
      $optionalMissing.Add(@{ name = $name; missing_targets = $missingTargets; errors = $targetErrors }) | Out-Null
    }
  }

  if ($StrictOptional -and $optionalMissing.Count -gt 0) {
    $lines = @()
    foreach ($m in $optionalMissing) {
      $t = @()
      if ($m.missing_targets) { $t = $m.missing_targets }
      $detail = ""
      if ($m.errors -and $m.errors.Count -gt 0) {
        $detail = " Details: " + (($m.errors.Keys | Sort-Object) | ForEach-Object { "$_=$($m.errors[$_])" } -join " | ")
      }
      $lines += ("- " + $m.name + ": missing for " + ($t -join ", ") + $detail)
    }
    $formatted = ($lines -join "`n")
    throw "One or more optional drivers were requested but are missing from the virtio-win source:`n$formatted`n`nHint: use a virtio-win ISO/root that includes Win7 audio/input drivers, re-run without -StrictOptional, or exclude optional drivers via -Drivers viostor,netkvm."
  }

  $createdUtc = (Get-Date).ToUniversalTime().ToString("o")
  $sourcePath = if ($isoPath) { $isoPath } else { $VirtioWinRoot }
  $sourceHash = if ($isoHash) { @{ algorithm = "sha256"; value = $isoHash } } else { $null }
  $derivedVersion = Derive-VirtioWinVersion -IsoPath $isoPath -VirtioRoot $VirtioWinRoot

  $manifest = [ordered]@{
    pack = "aero-win7-driver-pack"
    created_utc = $createdUtc
    source = [ordered]@{
      virtio_win_root = $VirtioWinRoot
      virtio_win_iso = $isoPath
      path = $sourcePath
      hash = $sourceHash
      volume_label = $isoVolumeLabel
      derived_version = $derivedVersion
      license_notice_files_copied = @($virtioWinNoticeFiles)
      timestamp_utc = $createdUtc
    }
    drivers_requested = @($requestedDrivers)
    drivers = @($includedDrivers | Sort-Object)
    optional_drivers_missing = @($optionalMissing)
    optional_drivers_missing_any = ($optionalMissing.Count -gt 0)
    warnings = @($warnings)
    driver_results = $driverResults
    targets = @("win7-x86", "win7-amd64")
  } | ConvertTo-Json -Depth 8

  $manifestPath = Join-Path $packRoot "manifest.json"
  # Write UTF-8 without BOM (PowerShell 5.1's `Out-File -Encoding UTF8` emits a BOM,
  # which breaks some strict JSON parsers).
  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllText($manifestPath, ($manifest + "`n"), $utf8NoBom)

  if (-not $NoZip) {
    $zipPath = Join-Path $out "aero-win7-driver-pack.zip"
    if (Test-Path $zipPath) {
      Remove-Item -Path $zipPath -Force
    }
    Compress-Archive -Path (Join-Path $packRoot "*") -DestinationPath $zipPath -Force
    Write-Host "Wrote $zipPath"
  } else {
    Write-Host "Wrote staging directory $packRoot"
  }
}
finally {
  if ($mounted -and $null -ne $isoPath) {
    Dismount-DiskImage -ImagePath $isoPath | Out-Null
  }
  if ($extractTempDir) {
    Remove-Item -LiteralPath $extractTempDir -Recurse -Force -ErrorAction SilentlyContinue
  }
}
