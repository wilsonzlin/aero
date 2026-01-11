# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Stages a ready-to-install Aero virtio-snd Windows 7 driver package under release/<arch>/.

.DESCRIPTION
  Copies the driver package payload files from `drivers/windows7/virtio-snd/inf/` into:

    release\<arch>\virtio-snd\

  This output can be copied directly into Guest Tools (guest-tools\drivers\<arch>\virtio-snd\)
  or used via Device Manager "Have Disk...".

  Optionally, the script can also produce a deterministic ZIP bundle (useful for shipping artifacts)
  when -Zip is specified.
#>

[CmdletBinding()]
param(
  [ValidateSet('auto', 'x86', 'amd64')]
  [string]$Arch = 'auto',

  [ValidateNotNullOrEmpty()]
  [string]$ReleaseRoot = (Join-Path $PSScriptRoot "..\\release"),

  [switch]$Zip,

  [ValidateNotNullOrEmpty()]
  [string]$OutDir = (Join-Path $PSScriptRoot "..\\release\\out")
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:DriverId = 'aero-virtio-snd'
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
      '^\\s*DriverVer\\s*=\\s*([^,]+)\\s*,\\s*([^;\\s]+)',
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

function Get-CatalogFileNamesFromInf([string]$InfPath) {
  $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
  $names = @()
  foreach ($line in $lines) {
    $m = [regex]::Match(
      $line,
      '^\\s*CatalogFile(\\.[^=\\s]+)?\\s*=\\s*([^\\s;]+\\.cat)\\b',
      [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($m.Success) {
      $names += $m.Groups[2].Value.Trim()
    }
  }

  return @($names | Select-Object -Unique)
}

function Get-PeMachine([string]$Path) {
  try {
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
  catch {
    return $null
  }
}

function Get-ArchFromSys([string]$SysPath) {
  $machine = Get-PeMachine -Path $SysPath
  if ($null -eq $machine) {
    throw "Could not read PE header from SYS: $SysPath"
  }
  switch ($machine) {
    0x014c { return 'x86' }   # IMAGE_FILE_MACHINE_I386
    0x8664 { return 'amd64' } # IMAGE_FILE_MACHINE_AMD64
    default { throw ("Could not determine SYS architecture (PE machine: 0x{0})." -f ("{0:x4}" -f $machine)) }
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

$infFiles = @(
  Get-ChildItem -LiteralPath $infDir -File -Filter '*.inf' | Sort-Object -Property Name
)
if ($infFiles.Count -eq 0) {
  throw "INF not found under: $infDir"
}

$preferred = @(
  (Join-Path $infDir 'aero-virtio-snd.inf'),
  (Join-Path $infDir 'virtio-snd.inf')
)
$infPath = $null
foreach ($p in $preferred) {
  if (Test-Path -LiteralPath $p -PathType Leaf) {
    $infPath = $p
    break
  }
}
if ($null -eq $infPath) {
  if ($infFiles.Count -eq 1) {
    $infPath = $infFiles[0].FullName
  }
  else {
    $list = $infFiles | ForEach-Object { "  - $($_.Name)" } | Out-String
    throw ("Multiple INF files found under {0}. Expected aero-virtio-snd.inf (preferred) or virtio-snd.inf.`r`nFound:`r`n{1}" -f $infDir, $list.TrimEnd())
  }
}

$sysPath = Join-Path $infDir $script:SysFileName
if (-not (Test-Path -LiteralPath $sysPath -PathType Leaf)) {
  throw "SYS not found: $sysPath`r`nBuild the driver and copy virtiosnd.sys into the inf\\ directory before packaging."
}

$detectedArch = Get-ArchFromSys -SysPath $sysPath
$resolvedArch = $Arch.ToLowerInvariant()
if ($resolvedArch -eq 'auto') {
  $resolvedArch = $detectedArch
}
elseif ($resolvedArch -ne $detectedArch) {
  throw ("-Arch {0} does not match SYS architecture ({1}). SYS: {2}" -f $resolvedArch, $detectedArch, $sysPath)
}

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
  throw "No payload files found to stage under: $infDir"
}

$missingCatalogs = @()
foreach ($inf in $infFiles) {
  $catNames = Get-CatalogFileNamesFromInf -InfPath $inf.FullName
  foreach ($catName in $catNames) {
    $catPath = Join-Path $infDir $catName
    if (-not (Test-Path -LiteralPath $catPath -PathType Leaf)) {
      $missingCatalogs += [pscustomobject]@{
        Inf = $inf.Name
        Cat = $catName
      }
    }
  }
}

if ($missingCatalogs.Count -gt 0) {
  $lines = $missingCatalogs | ForEach-Object { "  - {0} -> {1}" -f $_.Inf, $_.Cat }
  $detail = ($lines -join "`r`n")
  throw ("Missing catalog file(s) referenced by INF(s) under {0}:`r`n{1}`r`n`r`nRun scripts\\make-cat.cmd, then scripts\\sign-driver.cmd." -f $infDir, $detail)
}

$releaseRootResolved = Resolve-OrCreateDirectory -Path $ReleaseRoot -ArgName '-ReleaseRoot'
$stageDir = Join-Path $releaseRootResolved (Join-Path $resolvedArch 'virtio-snd')

if (Test-Path -LiteralPath $stageDir) {
  Remove-Item -LiteralPath $stageDir -Recurse -Force
}
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

foreach ($f in $payload) {
  Copy-Item -LiteralPath $f.FullName -Destination (Join-Path $stageDir $f.Name) -Force
}

Write-Host ("Staged driver package to: {0}" -f $stageDir)

if ($Zip) {
  $driverVer = Get-DriverVerFromInf -InfPath $infPath
  $outDirResolved = Resolve-OrCreateDirectory -Path $OutDir -ArgName '-OutDir'

  $zipName = ("{0}-{1}-{2}-{3}.zip" -f $script:DriverId, $script:TargetOs, $resolvedArch, $driverVer.version)
  $zipPath = Join-Path $outDirResolved $zipName

  $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("{0}-{1}-{2}" -f $script:DriverId, $script:TargetOs, [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

  try {
    foreach ($f in $payload) {
      Copy-Item -LiteralPath $f.FullName -Destination (Join-Path $tmpDir $f.Name) -Force
    }
    New-DeterministicZip -SourceDir $tmpDir -ZipPath $zipPath
  }
  finally {
    if (Test-Path -LiteralPath $tmpDir) {
      Remove-Item -LiteralPath $tmpDir -Recurse -Force
    }
  }

  Write-Host ("Created {0}" -f $zipPath)
}

