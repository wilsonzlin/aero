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

  $drivers = @(
    @{ Name = "viostor"; Upstream = "viostor" },
    @{ Name = "netkvm"; Upstream = "NetKVM" },
    @{ Name = "viosnd"; Upstream = "viosnd" },
    @{ Name = "vioinput"; Upstream = "vioinput" }
  )

  foreach ($drv in $drivers) {
    $name = $drv.Name
    $up = $drv.Upstream

    Write-Host "Packing $name (from $up)..."

    Copy-VirtioWinDriver -VirtioRoot $VirtioWinRoot -DriverDirName $up -OsDirCandidates $OsFolderCandidates -ArchCandidates $ArchCandidatesAmd64 -DestDir (Join-Path $win7Amd64 $name)
    Copy-VirtioWinDriver -VirtioRoot $VirtioWinRoot -DriverDirName $up -OsDirCandidates $OsFolderCandidates -ArchCandidates $ArchCandidatesX86 -DestDir (Join-Path $win7X86 $name)
  }

  $createdUtc = (Get-Date).ToUniversalTime().ToString("o")
  $sourcePath = if ($isoPath) { $isoPath } else { $VirtioWinRoot }
  $sourceHash = if ($isoHash) { @{ algorithm = "sha256"; value = $isoHash } } else { $null }
  $derivedVersion = Derive-VirtioWinVersion -IsoPath $isoPath -VirtioRoot $VirtioWinRoot

  $manifest = [ordered]@{
    pack = "aero-win7-driver-pack"
    created_utc = $createdUtc
    source = [ordered]@{
      path = $sourcePath
      hash = $sourceHash
      volume_label = $isoVolumeLabel
      derived_version = $derivedVersion
      timestamp_utc = $createdUtc
    }
    drivers = @("viostor", "netkvm", "viosnd", "vioinput")
    targets = @("win7-x86", "win7-amd64")
  } | ConvertTo-Json -Depth 6

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
