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
#   - heuristic for BOM-less UTF-16 (parity-biased NUL bytes over a few prefix windows, similar
#     to the packager's INF scanning)
#   - fallback to UTF-8 (ASCII-safe for the IDs we care about)
function Read-InfText {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  $bytes = [System.IO.File]::ReadAllBytes($Path)
  if (-not $bytes -or $bytes.Length -eq 0) { return '' }

  $text = $null

  # UTF-8 BOM: EF BB BF
  if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    $text = [System.Text.Encoding]::UTF8.GetString($bytes, 3, $bytes.Length - 3)
  }
  # UTF-16 LE BOM: FF FE
  elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
    $len = $bytes.Length - 2
    if (($len % 2) -ne 0) { $len-- }
    if ($len -le 0) { return '' }
    $text = [System.Text.Encoding]::Unicode.GetString($bytes, 2, $len)
  }
  # UTF-16 BE BOM: FE FF
  elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) {
    $len = $bytes.Length - 2
    if (($len % 2) -ne 0) { $len-- }
    if ($len -le 0) { return '' }
    $text = [System.Text.Encoding]::BigEndianUnicode.GetString($bytes, 2, $len)
  }
  # Heuristic for BOM-less UTF-16 (LE vs BE).
  elseif ($bytes.Length -ge 4 -and ($bytes.Length % 2) -eq 0) {
    # Similar to `tools/packaging/aero_packager`:
    # vote on endianness using a few prefix windows, since large Unicode-heavy sections (e.g.
    # string tables) can reduce the overall NUL-byte ratio even when the file is UTF-16.
    $nulRatioThreshold = 0.30
    $nulRatioSkew = 0.20
    $leVotes = 0
    $beVotes = 0

    foreach ($prefixLen in @(128, 512, 2048)) {
      $len = [Math]::Min($bytes.Length, $prefixLen)
      $len -= $len % 2
      if ($len -lt 4) { continue }

      $nulEven = 0
      $nulOdd = 0
      for ($i = 0; $i -lt $len; $i++) {
        if ($bytes[$i] -ne 0) { continue }
        if (($i % 2) -eq 0) { $nulEven++ } else { $nulOdd++ }
      }

      $half = [Math]::Max(1, [Math]::Floor($len / 2))
      $ratioEven = $nulEven / [double]$half
      $ratioOdd = $nulOdd / [double]$half

      if ($ratioOdd -ge $nulRatioThreshold -and ($ratioOdd - $ratioEven) -ge $nulRatioSkew) {
        $leVotes++
      } elseif ($ratioEven -ge $nulRatioThreshold -and ($ratioEven - $ratioOdd) -ge $nulRatioSkew) {
        $beVotes++
      }
    }

    if ($leVotes -eq 0 -and $beVotes -eq 0) {
      $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    } elseif ($leVotes -gt $beVotes) {
      $text = [System.Text.Encoding]::Unicode.GetString($bytes)
    } elseif ($beVotes -gt $leVotes) {
      $text = [System.Text.Encoding]::BigEndianUnicode.GetString($bytes)
    } else {
      # Ambiguous: decode both and pick the more text-like one.
      $le = [System.Text.Encoding]::Unicode.GetString($bytes)
      $be = [System.Text.Encoding]::BigEndianUnicode.GetString($bytes)

      function Get-DecodeScore([string]$s) {
        $replacement = 0
        $nul = 0
        $ascii = 0
        $newlines = 0
        $total = 0

        foreach ($c in $s.ToCharArray()) {
          $total++
          if ($c -eq [char]0xFFFD) {
            $replacement++
          } elseif ($c -eq [char]0x0000) {
            $nul++
          }

          if ([int]$c -lt 128) {
            $ascii++
            if ($c -eq "`n") { $newlines++ }
          }
        }

        return [pscustomobject]@{
          Replacement = $replacement
          Nul = $nul
          AsciiPenalty = $total - $ascii
          NewlinePenalty = $total - $newlines
        }
      }

      $leScore = Get-DecodeScore $le
      $beScore = Get-DecodeScore $be

      $chooseLe = $true
      if ($leScore.Replacement -lt $beScore.Replacement) { $chooseLe = $true }
      elseif ($leScore.Replacement -gt $beScore.Replacement) { $chooseLe = $false }
      elseif ($leScore.Nul -lt $beScore.Nul) { $chooseLe = $true }
      elseif ($leScore.Nul -gt $beScore.Nul) { $chooseLe = $false }
      elseif ($leScore.AsciiPenalty -lt $beScore.AsciiPenalty) { $chooseLe = $true }
      elseif ($leScore.AsciiPenalty -gt $beScore.AsciiPenalty) { $chooseLe = $false }
      elseif ($leScore.NewlinePenalty -lt $beScore.NewlinePenalty) { $chooseLe = $true }
      elseif ($leScore.NewlinePenalty -gt $beScore.NewlinePenalty) { $chooseLe = $false }

      # Prefer UTF-16LE when still tied (Windows commonly uses UTF-16LE).
      if ($chooseLe) { $text = $le } else { $text = $be }
    }
  } else {
    # Default: treat as UTF-8. Even for legacy ANSI, this is ASCII-safe for the `PCI\VEN_...` IDs.
    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
  }

  # Strip a BOM character if the decoder left one in.
  if ($text -and $text.Length -gt 0 -and $text[0] -eq [char]0xFEFF) {
    $text = $text.Substring(1)
  }
  return $text
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

    # Ensure BOM-less UTF-16 is still detected even if the overall file isn't very ASCII-heavy.
    # (The packager uses a prefix-based heuristic for this reason.)
    $unicodeTail = 'Ω' * 5000
    $sampleUnicode = $sample + "ExtraDesc=`"$unicodeTail`"`r`n"

    $pathLeUnicode = Join-Path $tmpDir 'utf16le_no_bom_unicode_tail.inf'
    [System.IO.File]::WriteAllBytes($pathLeUnicode, [System.Text.Encoding]::Unicode.GetBytes($sampleUnicode))
    if ((Read-InfText $pathLeUnicode) -ne $sampleUnicode) {
      throw "Read-InfText selftest failed (UTF-16LE no BOM, unicode tail)."
    }

    $pathBeUnicode = Join-Path $tmpDir 'utf16be_no_bom_unicode_tail.inf'
    [System.IO.File]::WriteAllBytes($pathBeUnicode, [System.Text.Encoding]::BigEndianUnicode.GetBytes($sampleUnicode))
    if ((Read-InfText $pathBeUnicode) -ne $sampleUnicode) {
      throw "Read-InfText selftest failed (UTF-16BE no BOM, unicode tail)."
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
