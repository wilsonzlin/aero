# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Packages the Aero virtio-snd Windows 7 driver into a deterministic zip.

.DESCRIPTION
  Collects the driver package payload files from `drivers/windows7/virtio-snd/inf/`
  and writes a deterministic zip into `drivers/windows7/virtio-snd/release/`.

  The script is intentionally simple: it packages whatever payload files exist in `inf/`
  (excluding repo metadata files like `.gitignore`), which makes it suitable for both
  signed and unsigned local builds.
#>

[CmdletBinding()]
param(
  [ValidateNotNullOrEmpty()]
  [string]$OutDir = (Join-Path $PSScriptRoot "..\\release\\out")
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:DriverId = 'aero-virtio-snd'
$script:InfFileName = 'virtio-snd.inf'
$script:SysFileName = 'virtiosnd.sys'
$script:TargetOs = 'win7'
$script:FixedZipTimestamp = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)

function Resolve-OrCreateDirectory([string]$Path, [string]$ArgName) {
  if (-not (Test-Path -LiteralPath $Path)) {
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
  }
  $resolved = Resolve-Path -LiteralPath $Path
  if (-not (Test-Path -LiteralPath $resolved.Path -PathType Container)) {
    throw "$ArgName is not a directory: $Path"
  }
  return $resolved.Path
}

function Get-DriverVerFromInf([string]$InfPath) {
  $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
  foreach ($line in $lines) {
    $m = [regex]::Match(
      $line,
      '^\s*DriverVer\s*=\s*([^,]+)\s*,\s*([^;\s]+)',
      [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($m.Success) {
      return [ordered]@{
        date = $m.Groups[1].Value.Trim()
        version = $m.Groups[2].Value.Trim()
      }
    }
  }
  throw "Could not find a DriverVer=...,... line in INF: $InfPath"
}

function Get-PeMachine([string]$Path) {
  $fs = [System.IO.File]::OpenRead($Path)
  try {
    $br = New-Object System.IO.BinaryReader($fs)
    try {
      if ($br.ReadUInt16() -ne 0x5A4D) { return $null } # MZ
      $fs.Seek(0x3C, [System.IO.SeekOrigin]::Begin) | Out-Null
      $peOffset = $br.ReadUInt32()
      $fs.Seek([int64]$peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
      if ($br.ReadUInt32() -ne 0x00004550) { return $null } # PE\0\0
      return $br.ReadUInt16()
    }
    finally {
      $br.Dispose()
    }
  }
  finally {
    $fs.Dispose()
  }
}

function Get-ArchFromSys([string]$SysPath) {
  $machine = Get-PeMachine -Path $SysPath
  switch ($machine) {
    0x014c { return 'x86' }   # IMAGE_FILE_MACHINE_I386
    0x8664 { return 'amd64' } # IMAGE_FILE_MACHINE_AMD64
    default { throw ("Could not determine SYS architecture (PE machine: {0})." -f $machine) }
  }
}

function New-DeterministicZip([string]$SourceDir, [string]$ZipPath) {
  Add-Type -AssemblyName System.IO.Compression | Out-Null
  Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

  if (Test-Path -LiteralPath $ZipPath) {
    Remove-Item -LiteralPath $ZipPath -Force
  }

  $files = @(
    Get-ChildItem -LiteralPath $SourceDir -File | Sort-Object -Property Name
  )

  $zipStream = [System.IO.File]::Open($ZipPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
  try {
    $zip = New-Object System.IO.Compression.ZipArchive(
      $zipStream,
      [System.IO.Compression.ZipArchiveMode]::Create,
      $false
    )
    try {
      foreach ($f in $files) {
        $entry = $zip.CreateEntry($f.Name, [System.IO.Compression.CompressionLevel]::Optimal)
        $entry.LastWriteTime = $script:FixedZipTimestamp
        $entryStream = $entry.Open()
        try {
          $fileStream = [System.IO.File]::OpenRead($f.FullName)
          try {
            $fileStream.CopyTo($entryStream)
          }
          finally {
            $fileStream.Dispose()
          }
        }
        finally {
          $entryStream.Dispose()
        }
      }
    }
    finally {
      $zip.Dispose()
    }
  }
  finally {
    $zipStream.Dispose()
  }
}

$virtioSndRoot = Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')
$infDir = Join-Path $virtioSndRoot.Path 'inf'

if (-not (Test-Path -LiteralPath $infDir -PathType Container)) {
  throw "INF directory not found: $infDir"
}

$infPath = Join-Path $infDir $script:InfFileName
if (-not (Test-Path -LiteralPath $infPath -PathType Leaf)) {
  throw "INF not found: $infPath"
}

$sysPath = Join-Path $infDir $script:SysFileName
if (-not (Test-Path -LiteralPath $sysPath -PathType Leaf)) {
  throw "SYS not found: $sysPath`r`nBuild the driver and copy virtiosnd.sys into the inf\\ directory before packaging."
}

$driverVer = Get-DriverVerFromInf -InfPath $infPath
$arch = Get-ArchFromSys -SysPath $sysPath

$outDirResolved = Resolve-OrCreateDirectory -Path $OutDir -ArgName '-OutDir'

$zipName = ("{0}-{1}-{2}-{3}.zip" -f $script:DriverId, $script:TargetOs, $arch, $driverVer.version)
$zipPath = Join-Path $outDirResolved $zipName

$excludeNames = @(
  '.gitignore',
  '.gitkeep',
  'README.md'
)

$payload = @(
  Get-ChildItem -LiteralPath $infDir -File |
    Where-Object { $excludeNames -notcontains $_.Name } |
    Sort-Object -Property Name
)

if ($payload.Count -eq 0) {
  throw "No payload files found to package under: $infDir"
}

$stageDir = Join-Path ([System.IO.Path]::GetTempPath()) ("{0}-{1}-{2}" -f $script:DriverId, $script:TargetOs, [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

try {
  foreach ($f in $payload) {
    Copy-Item -LiteralPath $f.FullName -Destination (Join-Path $stageDir $f.Name) -Force
  }

  New-DeterministicZip -SourceDir $stageDir -ZipPath $zipPath
}
finally {
  if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
  }
}

Write-Host ("Created {0}" -f $zipPath)

