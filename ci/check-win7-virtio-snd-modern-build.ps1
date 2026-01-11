#Requires -Version 5.1
#
# SPDX-License-Identifier: MIT OR Apache-2.0
#
<#
.SYNOPSIS
Ensures the Windows 7 virtio-snd driver project builds the modern virtio-pci backend by default.

.DESCRIPTION
Aero's virtio-snd Windows 7 PortCls/WaveRT driver must bind to the contract-v1
modern-only virtio-snd PCI IDs (DEV_1059&REV_01) and therefore must *not* build
the legacy virtio-pci I/O-port backend by default.

This script parses drivers/windows7/virtio-snd/virtio-snd.vcxproj and fails if
any known legacy transport compilation units are included.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RepoRoot {
  $ciDir = $PSScriptRoot
  if (-not $ciDir) {
    throw "Unable to determine script directory (PSScriptRoot is empty)."
  }
  return (Resolve-Path (Join-Path $ciDir '..')).Path
}

$repoRoot = Get-RepoRoot
$projPath = Join-Path $repoRoot 'drivers/windows7/virtio-snd/virtio-snd.vcxproj'

if (-not (Test-Path -LiteralPath $projPath -PathType Leaf)) {
  Write-Host "virtio-snd project file not found ($projPath); skipping modern-backend guard."
  exit 0
}

[xml]$xml = Get-Content -LiteralPath $projPath -Raw -ErrorAction Stop
$compileNodes = $xml.SelectNodes("//*[local-name()='ClCompile' and @Include]")
$includes = @()
foreach ($node in $compileNodes) {
  $inc = [string]$node.GetAttribute('Include')
  if (-not [string]::IsNullOrWhiteSpace($inc)) {
    $includes += $inc
  }
}

$legacySources = @(
  'src\backend_virtio_legacy.c',
  'src\aeroviosnd_hw.c',
  '..\virtio\common\src\virtio_pci_legacy.c',
  '..\virtio\common\src\virtio_queue.c'
)

$violations = @()
foreach ($legacy in $legacySources) {
  if ($includes -contains $legacy) {
    $violations += $legacy
  }
}

if ($violations.Count -gt 0) {
  Write-Host ""
  Write-Host "ERROR: drivers/windows7/virtio-snd/virtio-snd.vcxproj still includes legacy virtio-pci sources."
  Write-Host "The default virtio-snd build must use the modern virtio-pci transport (BAR0 MMIO + vendor caps)."
  Write-Host ""
  Write-Host "Legacy sources found in project:"
  foreach ($v in $violations) {
    Write-Host ("- {0}" -f $v)
  }
  throw "virtio-snd.vcxproj must not compile legacy virtio-pci sources."
}

Write-Host "OK: virtio-snd.vcxproj does not include legacy virtio-pci sources."

