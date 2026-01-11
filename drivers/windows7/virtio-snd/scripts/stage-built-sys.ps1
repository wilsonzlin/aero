# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Copies a built virtiosnd.sys from a WDK/MSBuild output directory into inf/.

.DESCRIPTION
  The signing workflow expects the driver binary to be staged next to the INF(s):

    drivers\windows7\virtio-snd\inf\virtiosnd.sys

  This helper searches recursively for `virtiosnd.sys` under an input directory
  (by default the driver root) and selects the one that matches the requested
  architecture based on the PE header machine field.

  This is a convenience script for reducing manual copy/paste friction; it does
  not build the driver itself.
#>

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('x86', 'amd64')]
  [string]$Arch,

  [ValidateNotNullOrEmpty()]
  [string]$InputDir = (Join-Path $PSScriptRoot '..'),

  [ValidateNotNullOrEmpty()]
  [string]$InfDir = (Join-Path $PSScriptRoot '..\\inf')
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Format-PathList([string[]]$Paths) {
  return ($Paths | ForEach-Object { "  - $_" }) -join "`r`n"
}

function Get-ExpectedPeMachine([ValidateSet('x86', 'amd64')] [string]$ArchValue) {
  switch ($ArchValue) {
    'x86' { return 0x014c }    # IMAGE_FILE_MACHINE_I386
    'amd64' { return 0x8664 }  # IMAGE_FILE_MACHINE_AMD64
    default { throw "Unhandled arch '$ArchValue'." }
  }
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

function Resolve-ExistingDirectory([string]$Path, [string]$ArgName) {
  if (-not (Test-Path -LiteralPath $Path)) {
    throw "$ArgName does not exist: $Path"
  }
  $resolved = Resolve-Path -LiteralPath $Path
  if (-not (Test-Path -LiteralPath $resolved.Path -PathType Container)) {
    throw "$ArgName is not a directory: $Path"
  }
  return $resolved.Path
}

$inputDirResolved = Resolve-ExistingDirectory -Path $InputDir -ArgName '-InputDir'
$infDirResolved = Resolve-ExistingDirectory -Path $InfDir -ArgName '-InfDir'

$expectedMachine = Get-ExpectedPeMachine -ArchValue $Arch

$candidates = @(
  Get-ChildItem -LiteralPath $inputDirResolved -Recurse -File -Filter 'virtiosnd.sys' -ErrorAction SilentlyContinue
)

if ($candidates.Count -eq 0) {
  throw "Could not find virtiosnd.sys under -InputDir '$inputDirResolved'. Build the driver first."
}

function Is-UnderDirectory([string]$Path, [string]$Dir) {
  $p = [System.IO.Path]::GetFullPath($Path).TrimEnd('\') + '\'
  $d = [System.IO.Path]::GetFullPath($Dir).TrimEnd('\') + '\'
  return $p.StartsWith($d, [System.StringComparison]::OrdinalIgnoreCase)
}

# Avoid selecting a pre-staged copy from inf/ or from previously staged release outputs.
$releaseDir = Join-Path (Split-Path -Parent $infDirResolved) 'release'
$filtered = @()
foreach ($c in $candidates) {
  if (Is-UnderDirectory -Path $c.FullName -Dir $infDirResolved) { continue }
  if (Test-Path -LiteralPath $releaseDir -PathType Container) {
    if (Is-UnderDirectory -Path $c.FullName -Dir $releaseDir) { continue }
  }
  $filtered += $c
}
if ($filtered.Count -gt 0) {
  $candidates = $filtered
}

$archMatches = @()
foreach ($c in $candidates) {
  $machine = Get-PeMachine -Path $c.FullName
  if ($machine -eq $expectedMachine) {
    $archMatches += $c
  }
}

if ($archMatches.Count -eq 0) {
  $paths = $candidates | ForEach-Object { $_.FullName }
  throw ("Found virtiosnd.sys under -InputDir '{0}', but none match architecture '{1}'. Candidates:`r`n{2}" -f $inputDirResolved, $Arch, (Format-PathList $paths))
}

if ($archMatches.Count -gt 1) {
  $paths = $archMatches | ForEach-Object { $_.FullName }
  throw ("Found multiple virtiosnd.sys builds matching architecture '{0}'. Clean old builds or point -InputDir at a single build output.`r`n{1}" -f $Arch, (Format-PathList $paths))
}

$srcPath = $archMatches[0].FullName
$dstPath = Join-Path $infDirResolved 'virtiosnd.sys'

Copy-Item -LiteralPath $srcPath -Destination $dstPath -Force

Write-Host "Staged driver binary:"
Write-Host ("  From: {0}" -f $srcPath)
Write-Host ("  To:   {0}" -f $dstPath)
Write-Host ""
Write-Host "Next:"
Write-Host "  1) scripts\\make-cat.cmd"
Write-Host "  2) scripts\\sign-driver.cmd"
Write-Host "  3) scripts\\package-release.ps1"

