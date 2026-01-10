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

try {
  if ($PSCmdlet.ParameterSetName -eq "FromIso") {
    $isoPath = (Resolve-Path $VirtioWinIso).Path
    $img = Mount-DiskImage -ImagePath $isoPath -PassThru
    $mounted = $true
    $vol = $img | Get-Volume
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

  $manifest = @{
    pack = "aero-win7-driver-pack"
    created_utc = (Get-Date).ToUniversalTime().ToString("o")
    source = @{
      virtio_win_root = $VirtioWinRoot
      virtio_win_iso = $isoPath
    }
    drivers = @("viostor", "netkvm", "viosnd", "vioinput")
    targets = @("win7-x86", "win7-amd64")
  } | ConvertTo-Json -Depth 4

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
