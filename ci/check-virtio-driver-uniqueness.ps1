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

The check ignores comments (lines starting with ';' and inline `; ...` comments) so
documentation inside INFs can still mention these IDs.

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

# INF files in the wild are commonly UTF-16LE (sometimes with no BOM). `Get-Content` does not
# reliably detect BOM-less UTF-16, causing this guardrail to silently miss HWIDs. Read the file
# as bytes and apply our own decoding:
#   - BOM detection for UTF-8 / UTF-16LE / UTF-16BE
#   - heuristic for BOM-less UTF-16 (look for NUL bytes biased to even/odd offsets)
#   - fallback to UTF-8 (ASCII-safe for the IDs we care about)
function Read-InfText {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  $bytes = [System.IO.File]::ReadAllBytes($Path)
  if (-not $bytes -or $bytes.Length -eq 0) { return '' }

  # UTF-8 BOM: EF BB BF
  if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    return [System.Text.Encoding]::UTF8.GetString($bytes, 3, $bytes.Length - 3)
  }

  # UTF-16 LE BOM: FF FE
  if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
    return [System.Text.Encoding]::Unicode.GetString($bytes, 2, $bytes.Length - 2)
  }

  # UTF-16 BE BOM: FE FF
  if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) {
    return [System.Text.Encoding]::BigEndianUnicode.GetString($bytes, 2, $bytes.Length - 2)
  }

  # Heuristic for BOM-less UTF-16:
  # UTF-16 INFs that are mostly ASCII tend to contain many NUL bytes, biased to either
  # even (UTF-16BE) or odd (UTF-16LE) byte offsets. This catches common BOM-less UTF-16 encodings
  # without accidentally treating regular UTF-8/ANSI INFs as UTF-16.
  if (($bytes.Length % 2) -eq 0) {
    $nulCount = 0
    $nulEven = 0
    $nulOdd = 0
    $sampleLen = $bytes.Length
    for ($i = 0; $i -lt $sampleLen; $i++) {
      if ($bytes[$i] -eq 0) {
        $nulCount++
        if (($i % 2) -eq 0) { $nulEven++ } else { $nulOdd++ }
      }
    }
    if ($nulCount -gt 0 -and $sampleLen -gt 0) {
      $half = [Math]::Max(1, [Math]::Floor($sampleLen / 2))
      $oddRatio = $nulOdd / [double]$half
      $evenRatio = $nulEven / [double]$half

      # Thresholds:
      # - In UTF-16(LE/BE), a noticeable fraction of bytes in the *high* (or low) byte position
      #   of UTF-16 code units will be NUL (particularly for ASCII syntax like section headers).
      # - Require a strong parity bias to avoid mis-detecting random NUL bytes.
      $biasedLe = ($nulOdd -gt 0 -and $nulOdd -ge ($nulEven * 2))
      $biasedBe = ($nulEven -gt 0 -and $nulEven -ge ($nulOdd * 2))

      if (($oddRatio -ge 0.05 -and $biasedLe) -or ($evenRatio -ge 0.05 -and $biasedBe)) {
        if ($biasedLe) {
          return [System.Text.Encoding]::Unicode.GetString($bytes)
        } else {
          return [System.Text.Encoding]::BigEndianUnicode.GetString($bytes)
        }
      }
    }
  }

  # Default: treat as UTF-8. Even for legacy ANSI, this is ASCII-safe for the `PCI\VEN_...` IDs.
  return [System.Text.Encoding]::UTF8.GetString($bytes)
}

# Optional unit-like selftest:
#   $env:AERO_CHECK_VIRTIO_UNIQUENESS_SELFTEST=1; powershell -File ci/check-virtio-driver-uniqueness.ps1
if ($env:AERO_CHECK_VIRTIO_UNIQUENESS_SELFTEST -eq '1') {
  $sample = "[Version]`r`nPCI\VEN_1AF4&DEV_1042`r`n"
  $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("aero-virtio-inf-encoding-" + [System.Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmpDir | Out-Null
  try {
    $pathLe = Join-Path $tmpDir 'utf16le_no_bom.inf'
    [System.IO.File]::WriteAllBytes($pathLe, [System.Text.Encoding]::Unicode.GetBytes($sample))
    if ((Read-InfText $pathLe) -ne $sample) {
      throw "Read-InfText selftest failed (UTF-16LE no BOM)."
    }

    $pathBe = Join-Path $tmpDir 'utf16be_no_bom.inf'
    [System.IO.File]::WriteAllBytes($pathBe, [System.Text.Encoding]::BigEndianUnicode.GetBytes($sample))
    if ((Read-InfText $pathBe) -ne $sample) {
      throw "Read-InfText selftest failed (UTF-16BE no BOM)."
    }
  } finally {
    Remove-Item -LiteralPath $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
  }
  Write-Host "OK: Read-InfText selftest passed."
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
  $text = Read-InfText $inf.FullName
  $lines = @($text -split "`r?`n")
  for ($i = 0; $i -lt $lines.Count; $i++) {
    $line = [string]$lines[$i]
    # INF comments start with ';' and run to end-of-line. Strip any inline comment
    # content first so documentation text doesn't get treated as a real HWID match.
    $uncommented = $line.Split(';', 2)[0].Trim()
    if ([string]::IsNullOrWhiteSpace($uncommented)) { continue }

    foreach ($p in $hwidPatterns) {
      if (-not $p.Regex.IsMatch($uncommented)) { continue }
      $matchesByPattern[$p.Name].Add([pscustomobject]@{
        Pattern = $p.Name
        Path = $inf.FullName
        Line = $i + 1
        Text = $uncommented
        OriginalText = $line
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
      if ($null -ne $e.OriginalText -and $e.OriginalText -ne $e.Text) {
        Write-Host ("        (original line, comments stripped): {0}" -f $e.OriginalText)
      }
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
