[CmdletBinding(DefaultParameterSetName = "FromRoot")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "FromIso")]
  [string]$VirtioWinIso,

  [Parameter(Mandatory = $true, ParameterSetName = "FromRoot")]
  [string]$VirtioWinRoot,

  [string]$OutDir = (Join-Path $PSScriptRoot "..\out"),

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

try {
  if ($PSCmdlet.ParameterSetName -eq "FromIso") {
    $isoPath = (Resolve-Path $VirtioWinIso).Path
    $isoHash = (Get-FileHash -Algorithm SHA256 -Path $isoPath).Hash.ToLowerInvariant()
    $img = Mount-DiskImage -ImagePath $isoPath -PassThru
    $mounted = $true
    $vol = $img | Get-Volume
    $isoVolumeLabel = $vol.FileSystemLabel
    $VirtioWinRoot = "$($vol.DriveLetter):\"
  } else {
    $VirtioWinRoot = (Resolve-Path $VirtioWinRoot).Path
  }

  if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
  }
  $out = (Resolve-Path $OutDir).Path

  $packRoot = Join-Path $out "aero-win7-driver-pack"
  if (Test-Path $packRoot) {
    Remove-Item -Path $packRoot -Recurse -Force
  }

  $win7Amd64 = Join-Path $packRoot "win7\amd64"
  $win7X86 = Join-Path $packRoot "win7\x86"

  New-Item -ItemType Directory -Path $win7Amd64 -Force | Out-Null
  New-Item -ItemType Directory -Path $win7X86 -Force | Out-Null

  Copy-Item -Path (Join-Path $PSScriptRoot "install.cmd") -Destination (Join-Path $packRoot "install.cmd") -Force
  Copy-Item -Path (Join-Path $PSScriptRoot "enable-testsigning.cmd") -Destination (Join-Path $packRoot "enable-testsigning.cmd") -Force

  $noticesSrc = Join-Path $PSScriptRoot "..\virtio\THIRD_PARTY_NOTICES.md"
  if (-not (Test-Path -LiteralPath $noticesSrc -PathType Leaf)) {
    throw "Expected third-party notices file not found: $noticesSrc"
  }
  Copy-Item -LiteralPath $noticesSrc -Destination (Join-Path $packRoot "THIRD_PARTY_NOTICES.md") -Force

  $virtioReadmeSrc = Join-Path $PSScriptRoot "..\virtio\README.md"
  if (Test-Path -LiteralPath $virtioReadmeSrc -PathType Leaf) {
    Copy-Item -LiteralPath $virtioReadmeSrc -Destination (Join-Path $packRoot "README.md") -Force
  }

  $driverResults = @()
  $includedDrivers = New-Object "System.Collections.Generic.HashSet[string]"
  $optionalMissing = New-Object "System.Collections.Generic.List[object]"
  $warnings = New-Object "System.Collections.Generic.List[string]"

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

  $manifest | Out-File -FilePath (Join-Path $packRoot "manifest.json") -Encoding UTF8

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
}
