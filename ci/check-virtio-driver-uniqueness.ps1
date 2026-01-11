#Requires -Version 5.1

<#
.SYNOPSIS
Guard against non-deterministic virtio driver binding (duplicate INFs / outputs).

.DESCRIPTION
Windows selects PnP drivers by matching a device's Hardware IDs against installed INFs.
If multiple INFs match the same virtio device ID (e.g. both bind `PCI\VEN_1AF4&DEV_1042`),
binding can become nondeterministic.

This script scans all driver INFs under `drivers/` and fails if more than one INF
references one of Aero's contract-v1 modern virtio PCI device IDs:

  - `PCI\VEN_1AF4&DEV_1041` (virtio-net)
  - `PCI\VEN_1AF4&DEV_1042` (virtio-blk)
  - `PCI\VEN_1AF4&DEV_1052` (virtio-input)
  - `PCI\VEN_1AF4&DEV_1059` (virtio-snd)

The check ignores comment lines (starting with ';') so documentation inside INFs can
still mention these IDs.

Additionally, enforce that there is only one MSBuild driver project under `drivers/`
that produces `aero_virtio_blk.sys` (TargetName = aero_virtio_blk) and only one
that produces `aero_virtio_net.sys` (TargetName = aero_virtio_net).

Also enforce uniqueness for the remaining contract-v1 virtio drivers:

- `aero_virtio_input.sys` (TargetName = aero_virtio_input)
- `aero_virtio_snd.sys` (TargetName = aero_virtio_snd)
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
$driversRoot = Join-Path $repoRoot 'drivers'

if (-not (Test-Path -LiteralPath $driversRoot -PathType Container)) {
  Write-Host "drivers/ directory not found ($driversRoot); skipping virtio INF HWID check."
  exit 0
}

$infFiles = @(Get-ChildItem -LiteralPath $driversRoot -Recurse -File -Filter '*.inf' -ErrorAction SilentlyContinue | Sort-Object -Property FullName)
if (-not $infFiles -or $infFiles.Count -eq 0) {
  Write-Host "No .inf files found under drivers/ ($driversRoot); skipping virtio INF HWID check."
  exit 0
}

# Only enforce the IDs we know are boot/perf-critical today; broaden later if needed.
$hwidPatterns = @(
  [pscustomobject]@{ Name = 'virtio-net (DEV_1041)'; Regex = [regex]::new('(?i)PCI\\VEN_1AF4&DEV_1041') },
  [pscustomobject]@{ Name = 'virtio-blk (DEV_1042)'; Regex = [regex]::new('(?i)PCI\\VEN_1AF4&DEV_1042') },
  [pscustomobject]@{ Name = 'virtio-input (DEV_1052)'; Regex = [regex]::new('(?i)PCI\\VEN_1AF4&DEV_1052') },
  [pscustomobject]@{ Name = 'virtio-snd (DEV_1059)'; Regex = [regex]::new('(?i)PCI\\VEN_1AF4&DEV_1059') }
)

$matchesByPattern = @{}
foreach ($p in $hwidPatterns) {
  $matchesByPattern[$p.Name] = New-Object System.Collections.Generic.List[object]
}

foreach ($inf in $infFiles) {
  $lines = @(Get-Content -LiteralPath $inf.FullName -ErrorAction Stop)
  for ($i = 0; $i -lt $lines.Count; $i++) {
    $line = [string]$lines[$i]
    $trim = $line.TrimStart()
    if ($trim.StartsWith(';')) { continue }

    foreach ($p in $hwidPatterns) {
      if (-not $p.Regex.IsMatch($line)) { continue }
      $matchesByPattern[$p.Name].Add([pscustomobject]@{
        Pattern = $p.Name
        Path = $inf.FullName
        Line = $i + 1
        Text = $line
      }) | Out-Null
      break
    }
  }
}

$conflicts = New-Object System.Collections.Generic.List[object]
foreach ($p in $hwidPatterns) {
  $entries = @(
    $matchesByPattern[$p.Name] |
      Group-Object -Property Path |
      ForEach-Object { $_.Group[0] }
  )
  if ($entries.Count -gt 1) {
    $conflicts.Add([pscustomobject]@{
      Pattern = $p.Name
      Entries = $entries
    }) | Out-Null
  }
}

if ($conflicts.Count -gt 0) {
  Write-Host ""
  Write-Host "ERROR: Multiple INFs match the same contract-v1 virtio PCI hardware IDs."
  Write-Host "Remove/disable the duplicate INFs so Windows cannot pick an unintended driver."
  Write-Host ""
  Write-Host "Conflicts:"
  foreach ($c in $conflicts) {
    Write-Host ("- {0}" -f $c.Pattern)
    foreach ($e in $c.Entries) {
      Write-Host ("    - {0}:{1}: {2}" -f $e.Path, $e.Line, $e.Text)
    }
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: no duplicate INFs match Aero contract-v1 virtio HWIDs (DEV_1041/DEV_1042/DEV_1052/DEV_1059)."

# Enforce that there is only one MSBuild project that produces aero_virtio_blk.sys / aero_virtio_net.sys.
$projFiles = @(Get-ChildItem -LiteralPath $driversRoot -Recurse -File -Filter '*.vcxproj' -ErrorAction SilentlyContinue | Sort-Object -Property FullName)
$virtioBlkProjects = New-Object System.Collections.Generic.List[string]
$virtioNetProjects = New-Object System.Collections.Generic.List[string]
$virtioInputProjects = New-Object System.Collections.Generic.List[string]
$virtioSndProjects = New-Object System.Collections.Generic.List[string]
foreach ($proj in $projFiles) {
  $content = $null
  try {
    $content = Get-Content -LiteralPath $proj.FullName -Raw -ErrorAction Stop
  } catch {
    continue
  }
  if ([string]::IsNullOrWhiteSpace($content)) { continue }
  if ($content -match '(?i)<TargetName>\s*aero_virtio_blk\s*</TargetName>') {
    $virtioBlkProjects.Add($proj.FullName) | Out-Null
  }
  if ($content -match '(?i)<TargetName>\s*aero_virtio_net\s*</TargetName>') {
    $virtioNetProjects.Add($proj.FullName) | Out-Null
  }
  if ($content -match '(?i)<TargetName>\s*aero_virtio_input\s*</TargetName>') {
    $virtioInputProjects.Add($proj.FullName) | Out-Null
  }
  if ($content -match '(?i)<TargetName>\s*aero_virtio_snd\s*</TargetName>') {
    $virtioSndProjects.Add($proj.FullName) | Out-Null
  }
}

$uniqueProjPaths = @($virtioBlkProjects | Sort-Object -Unique)
if ($uniqueProjPaths.Count -gt 1) {
  Write-Host ""
  Write-Host "ERROR: Found multiple MSBuild projects that produce aero_virtio_blk.sys (TargetName=aero_virtio_blk)."
  foreach ($p in $uniqueProjPaths) {
    Write-Host ("- {0}" -f $p)
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: aero_virtio_blk MSBuild output is unique."

$uniqueNetProjPaths = @($virtioNetProjects | Sort-Object -Unique)
if ($uniqueNetProjPaths.Count -gt 1) {
  Write-Host ""
  Write-Host "ERROR: Found multiple MSBuild projects that produce aero_virtio_net.sys (TargetName=aero_virtio_net)."
  foreach ($p in $uniqueNetProjPaths) {
    Write-Host ("- {0}" -f $p)
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: aero_virtio_net MSBuild output is unique."

$uniqueInputProjPaths = @($virtioInputProjects | Sort-Object -Unique)
if ($uniqueInputProjPaths.Count -gt 1) {
  Write-Host ""
  Write-Host "ERROR: Found multiple MSBuild projects that produce aero_virtio_input.sys (TargetName=aero_virtio_input)."
  foreach ($p in $uniqueInputProjPaths) {
    Write-Host ("- {0}" -f $p)
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: aero_virtio_input MSBuild output is unique."

$uniqueSndProjPaths = @($virtioSndProjects | Sort-Object -Unique)
if ($uniqueSndProjPaths.Count -gt 1) {
  Write-Host ""
  Write-Host "ERROR: Found multiple MSBuild projects that produce aero_virtio_snd.sys (TargetName=aero_virtio_snd)."
  foreach ($p in $uniqueSndProjPaths) {
    Write-Host ("- {0}" -f $p)
  }
  Write-Host ""
  exit 1
}

Write-Host "OK: aero_virtio_snd MSBuild output is unique."
exit 0
