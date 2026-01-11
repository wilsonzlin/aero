#Requires -Version 5.1

<#
.SYNOPSIS
Guard against INF hardware ID conflicts between legacy/transitional virtio drivers and
contract-v1 modern-only drivers.

.DESCRIPTION
The legacy/transitional driver packages under:

  drivers/windows7/virtio/

must NOT match Aero's contract-v1 modern-only virtio PCI Device ID space (`DEV_104x` / `DEV_105x`),
otherwise Windows may have multiple installed INFs that match the same modern device.

This script scans all *.inf files under that legacy directory and fails if it finds any
non-comment line that references a virtio modern-only hardware ID, e.g.:

  PCI\VEN_1AF4&DEV_1042

It intentionally ignores comment lines (starting with ';') so maintainers can still
reference modern IDs in explanatory comments without breaking the guard.
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
$legacyRoot = Join-Path (Join-Path (Join-Path $repoRoot 'drivers') 'windows7') 'virtio'

if (-not (Test-Path -LiteralPath $legacyRoot -PathType Container)) {
  Write-Host "Legacy virtio directory not found ($legacyRoot); skipping INF HWID check."
  exit 0
}

$infFiles = @(Get-ChildItem -LiteralPath $legacyRoot -Recurse -File -Filter '*.inf' -ErrorAction SilentlyContinue | Sort-Object -Property FullName)
if (-not $infFiles -or $infFiles.Count -eq 0) {
  Write-Host "No .inf files found under legacy virtio directory ($legacyRoot); skipping INF HWID check."
  exit 0
}

# Match virtio Vendor ID (0x1AF4) with the modern-only virtio-pci ID space (0x1040+ / 0x1050+).
# We only care about preventing accidental binding conflicts on Aero contract-v1 devices.
$modernHwidRegex = [regex]::new('(?i)PCI\\VEN_1AF4&DEV_10(4|5)[0-9A-F]')

$violations = New-Object System.Collections.Generic.List[object]

foreach ($inf in $infFiles) {
  $lines = @(Get-Content -LiteralPath $inf.FullName -ErrorAction Stop)
  for ($i = 0; $i -lt $lines.Count; $i++) {
    $line = [string]$lines[$i]
    $trim = $line.TrimStart()
    if ($trim.StartsWith(';')) { continue }
    if (-not $modernHwidRegex.IsMatch($line)) { continue }

    $violations.Add([pscustomobject]@{
      Path = $inf.FullName
      Line = $i + 1
      Text = $line
    }) | Out-Null
  }
}

if ($violations.Count -gt 0) {
  Write-Host ""
  Write-Host "ERROR: Found modern-only virtio PCI HWIDs referenced by legacy/transitional INFs under drivers/windows7/virtio/."
  Write-Host "These INFs must not bind to DEV_104x/DEV_105x because contract-v1 drivers also bind to those IDs."
  Write-Host ""
  Write-Host "Violations:"
  foreach ($v in $violations) {
    Write-Host ("- {0}:{1}: {2}" -f $v.Path, $v.Line, $v.Text)
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: legacy/transitional virtio INFs do not reference modern-only HWIDs (DEV_104x/DEV_105x)."
exit 0
