# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Statically validates aero_virtio_input.inf against Aero contract expectations.

.DESCRIPTION
  This is a lightweight (regex/string based) validator intended to catch accidental INF
  edits that would break Aero's Windows 7 virtio-input contract:

  - Must bind as HIDClass
  - Must reference the expected catalog filename
  - Must target KMDF 1.9 (in-box on Win7 SP1)
  - Must include the contract v1 keyboard/mouse HWID set (revision gated, REV_01)
  - Canonical INF (`aero_virtio_input.inf`) is intentionally SUBSYS-only (no strict generic fallback HWID).
  - Legacy alias INF (`virtio-input.inf{,.disabled}`) adds an opt-in strict, REV-qualified generic fallback HWID
    (no SUBSYS): `PCI\VEN_1AF4&DEV_1052&REV_01`
  - Legacy alias drift policy: outside the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`), from the first section
    header (`[Version]`) onward, the legacy filename alias must remain byte-for-byte identical to the canonical INF
    (only the leading banner/comments may differ). See `check-inf-alias.py`.
  - Must not include a revision-less base HWID (`PCI\VEN_1AF4&DEV_1052`) (revision gating is required)
  - Must use distinct DeviceDesc strings for keyboard vs mouse (so they appear separately in Device Manager)
  - Must enable MSI/MSI-X and request enough message interrupts for virtio-input

  By default, validates:
    ..\inf\aero_virtio_input.inf

.PARAMETER InfPath
  Path to the INF file to validate.
#>

[CmdletBinding()]
param(
  [ValidateNotNullOrEmpty()]
  [string]$InfPath = (Join-Path (Join-Path (Join-Path $PSScriptRoot '..') 'inf') 'aero_virtio_input.inf')
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Resolve-ExistingFile([string]$Path, [string]$ArgName) {
  if (-not (Test-Path -LiteralPath $Path)) {
    throw ("{0} does not exist: {1}" -f $ArgName, $Path)
  }
  $resolved = Resolve-Path -LiteralPath $Path
  if (-not (Test-Path -LiteralPath $resolved.Path -PathType Leaf)) {
    throw ("{0} is not a file: {1}" -f $ArgName, $Path)
  }
  return $resolved.Path
}

function Read-InfLines([string]$Path) {
  # INF files are commonly ASCII/ANSI or UTF-16LE. Some INFs are UTF-16LE *without* a BOM,
  # which PowerShell's default encoding heuristics may misinterpret. Decode the file bytes
  # ourselves so the validator remains robust across typical INF encodings.
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  if ($bytes.Length -eq 0) {
    return @()
  }

  $encoding = $null
  $offset = 0

  if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    $encoding = [System.Text.Encoding]::UTF8
    $offset = 3
  }
  elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
    $encoding = [System.Text.Encoding]::Unicode # UTF-16LE
    $offset = 2
  }
  elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) {
    $encoding = [System.Text.Encoding]::BigEndianUnicode # UTF-16BE
    $offset = 2
  }
  else {
    # Heuristic detection for UTF-16 without BOM: plain ASCII UTF-16LE will have many 0x00
    # bytes at odd indices; UTF-16BE will have many 0x00 bytes at even indices.
    $sampleLen = [Math]::Min($bytes.Length, 4096)
    $evenZeros = 0
    $oddZeros = 0
    for ($i = 0; $i -lt $sampleLen; $i++) {
      if ($bytes[$i] -eq 0) {
        if (($i % 2) -eq 0) { $evenZeros++ } else { $oddZeros++ }
      }
    }

    if ($oddZeros -gt ($evenZeros * 4 + 10)) {
      $encoding = [System.Text.Encoding]::Unicode
    }
    elseif ($evenZeros -gt ($oddZeros * 4 + 10)) {
      $encoding = [System.Text.Encoding]::BigEndianUnicode
    }
    else {
      # Prefer UTF-8 when the bytes are valid UTF-8, otherwise fall back to the platform
      # default (ANSI codepage on Windows PowerShell).
      $utf8Strict = New-Object System.Text.UTF8Encoding($false, $true)
      try {
        $text = $utf8Strict.GetString($bytes)
        return [regex]::Split($text, "\r\n|\n|\r")
      }
      catch {
        $encoding = [System.Text.Encoding]::Default
      }
    }
  }

  $count = $bytes.Length - $offset
  if ($count -lt 0) { $count = 0 }
  $text = $encoding.GetString($bytes, $offset, $count)
  if ($text.Length -gt 0 -and $text[0] -eq [char]0xFEFF) {
    $text = $text.Substring(1)
  }

  return [regex]::Split($text, "\r\n|\n|\r")
}

function Get-FirstNonblankAsciiByte([byte[]]$Line, [bool]$FirstLine) {
  # Return the first meaningful ASCII byte on a line, ignoring whitespace and NULs.
  #
  # This is robust to UTF-16LE/BE encoded INFs where each ASCII character may be
  # separated by a NUL byte.
  $start = 0
  if ($FirstLine) {
    # Strip BOMs for *detection only*. Returned content still includes them.
    if ($Line.Length -ge 3 -and $Line[0] -eq 0xEF -and $Line[1] -eq 0xBB -and $Line[2] -eq 0xBF) {
      $start = 3
    }
    elseif ($Line.Length -ge 2 -and (($Line[0] -eq 0xFF -and $Line[1] -eq 0xFE) -or ($Line[0] -eq 0xFE -and $Line[1] -eq 0xFF))) {
      $start = 2
    }
  }

  for ($i = $start; $i -lt $Line.Length; $i++) {
    $b = $Line[$i]
    if ($b -in 0x00, 0x09, 0x0A, 0x0D, 0x20) {
      continue
    }
    return $b
  }
  return $null
}

function Inf-BytesFromFirstSection([string]$Path) {
  # Return the file content starting from the first section header line (typically `[Version]`).
  #
  # We intentionally ignore the leading banner/comments so the legacy alias INF can have a different
  # filename banner, while still enforcing byte-for-byte equality for all functional content.
  $data = [System.IO.File]::ReadAllBytes($Path)
  if ($data.Length -eq 0) {
    throw ("INF is empty: {0}" -f $Path)
  }

  $lineStart = 0
  $lineIndex = 0
  while ($lineStart -lt $data.Length) {
    # Find end-of-line (including the newline byte, if present).
    $i = $lineStart
    while ($i -lt $data.Length -and $data[$i] -ne 0x0A) {
      $i++
    }
    $nextStart = if ($i -lt $data.Length -and $data[$i] -eq 0x0A) { $i + 1 } else { $i }

    $len = $nextStart - $lineStart
    $line = New-Object byte[] $len
    [System.Array]::Copy($data, $lineStart, $line, 0, $len)

    $first = Get-FirstNonblankAsciiByte -Line $line -FirstLine ($lineIndex -eq 0)
    if ($null -ne $first) {
      # First section header (e.g. "[Version]") starts the compared region.
      if ($first -eq 0x5B) {
        $outLen = $data.Length - $lineStart
        $out = New-Object byte[] $outLen
        [System.Array]::Copy($data, $lineStart, $out, 0, $outLen)
        return $out
      }

      # Ignore leading comments.
      if ($first -eq 0x3B) {
        $lineStart = $nextStart
        $lineIndex++
        continue
      }

      # Unexpected preamble content (not comment, not blank, not section): treat it as part of the
      # compared region to avoid masking drift.
      $outLen = $data.Length - $lineStart
      $out = New-Object byte[] $outLen
      [System.Array]::Copy($data, $lineStart, $out, 0, $outLen)
      return $out
    }

    $lineStart = $nextStart
    $lineIndex++
  }

  throw ("INF {0}: could not find a section header line (e.g. [Version])" -f $Path)
}

function Strip-InfSectionsBytes([byte[]]$Data, [string[]]$DropSections) {
  # Remove entire INF sections (including their headers) by name (case-insensitive).
  #
  # Note: this helper can be used by INF drift checks that want to ignore specific
  # sections when comparing two INFs.
  $drop = @{}
  foreach ($s in $DropSections) {
    if ($null -ne $s) { $drop[$s.ToLowerInvariant()] = $true }
  }

  $out = New-Object System.Collections.Generic.List[byte]
  $skipping = $false

  $lineStart = 0
  while ($lineStart -lt $Data.Length) {
    # Find end-of-line (including the newline byte, if present).
    $i = $lineStart
    while ($i -lt $Data.Length -and $Data[$i] -ne 0x0A) { $i++ }
    $nextStart = if ($i -lt $Data.Length -and $Data[$i] -eq 0x0A) { $i + 1 } else { $i }

    $len = $nextStart - $lineStart
    $line = New-Object byte[] $len
    [System.Array]::Copy($Data, $lineStart, $line, 0, $len)

    # Detection-only ASCII view (strip NULs).
    #
    # Note: cast to [byte[]] so Array.Copy into $nameBytes works (PowerShell otherwise
    # materializes an object[]).
    $lineAscii = [byte[]]($line | Where-Object { $_ -ne 0x00 })

    # Trim leading ASCII whitespace for header detection.
    $j = 0
    while ($j -lt $lineAscii.Length -and ($lineAscii[$j] -eq 0x20 -or $lineAscii[$j] -eq 0x09 -or $lineAscii[$j] -eq 0x0D -or $lineAscii[$j] -eq 0x0A)) {
      $j++
    }

    if ($j -lt $lineAscii.Length -and $lineAscii[$j] -eq 0x5B) { # '['
      $end = -1
      for ($k = $j + 1; $k -lt $lineAscii.Length; $k++) {
        if ($lineAscii[$k] -eq 0x5D) { $end = $k; break } # ']'
      }
      if ($end -gt ($j + 1)) {
        $nameBytes = New-Object byte[] ($end - ($j + 1))
        [System.Array]::Copy($lineAscii, $j + 1, $nameBytes, 0, $nameBytes.Length)
        $name = [System.Text.Encoding]::UTF8.GetString($nameBytes).Trim()
        $skipping = $drop.ContainsKey($name.ToLowerInvariant())
      }
    }

    if (-not $skipping) {
      $out.AddRange($line)
    }

    $lineStart = $nextStart
  }

  return $out.ToArray()
}

function Strip-InfComments([string]$Line) {
  # INF comments begin with ';' outside of quoted strings.
  $inQuote = $false
  for ($i = 0; $i -lt $Line.Length; $i++) {
    $ch = $Line[$i]
    if ($ch -eq '"') {
      $inQuote = -not $inQuote
      continue
    }
    if (-not $inQuote -and $ch -eq ';') {
      return $Line.Substring(0, $i)
    }
  }
  return $Line
}

function Parse-InfInteger([string]$Text) {
  $t = $Text.Trim()
  if ($t -match '^0x([0-9a-fA-F]+)$') {
    return [Convert]::ToInt32($Matches[1], 16)
  }
  if ($t -match '^[0-9]+$') {
    return [Convert]::ToInt32($t, 10)
  }
  throw ("Unable to parse integer value '{0}'." -f $Text)
}

function Unquote-InfString([string]$Text) {
  $t = $Text.Trim()
  if ($t.Length -ge 2 -and $t.StartsWith('"') -and $t.EndsWith('"')) {
    return $t.Substring(1, $t.Length - 2)
  }
  return $t
}

function Split-InfCommaList([string]$Text) {
  # INF comma-separated lists do not support quoting/escaping in our usage here.
  $items = @($Text.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_.Length -gt 0 })
  return $items
}

function Get-MatchingLines([System.Collections.IEnumerable]$Lines, [string]$Regex) {
  return @($Lines | Where-Object { $_ -match $Regex })
}

function Add-Failure([System.Collections.Generic.List[string]]$Failures, [string]$Message) {
  if (-not $Failures.Contains($Message)) {
    [void]$Failures.Add($Message)
  }
}

$exitCode = 0
try {
  $infPathResolved = Resolve-ExistingFile -Path $InfPath -ArgName '-InfPath'
  $rawLines = Read-InfLines -Path $infPathResolved
  $lines = New-Object System.Collections.Generic.List[string]
  $sections = @{}
  $currentSection = $null

  foreach ($l in $rawLines) {
    $stripped = Strip-InfComments -Line $l
    $trimmed = $stripped.Trim()
    if ($trimmed.Length -eq 0) { continue }

    $lines.Add($trimmed)

    if ($trimmed -match '^\[(?<name>[^\]]+)\]$') {
      $currentSection = $Matches['name'].Trim()
      if (-not $sections.ContainsKey($currentSection)) {
        $sections[$currentSection] = (New-Object System.Collections.Generic.List[string])
      }
      continue
    }

    if ($null -ne $currentSection) {
      # INF syntax allows the same section name to appear multiple times. Coalesce.
      if (-not $sections.ContainsKey($currentSection)) {
        $sections[$currentSection] = (New-Object System.Collections.Generic.List[string])
      }
      $sections[$currentSection].Add($trimmed)
    }
  }

  $failures = New-Object System.Collections.Generic.List[string]

  #------------------------------------------------------------------------------
  # Legacy filename alias drift guardrail (optional)
  #------------------------------------------------------------------------------
  # The repo may contain an optional legacy filename alias INF (`virtio-input.inf{,.disabled}`).
  # Policy: if present alongside the canonical INF, it is a legacy filename alias only.
  # - It is allowed to diverge from the canonical INF only in the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`)
  #   to add the opt-in strict generic fallback HWID.
  # - Outside those models sections, from the first section header (`[Version]`) onward, it must remain byte-for-byte
  #   identical to the canonical INF (only the leading banner/comments may differ).
  $infDir = Split-Path -Parent $infPathResolved
  $canonicalInf = Join-Path $infDir 'aero_virtio_input.inf'
  $aliasEnabled = Join-Path $infDir 'virtio-input.inf'
  $aliasDisabled = Join-Path $infDir 'virtio-input.inf.disabled'

  $aliasCandidates = @()
  if (Test-Path -LiteralPath $aliasEnabled -PathType Leaf) { $aliasCandidates += @($aliasEnabled) }
  if (Test-Path -LiteralPath $aliasDisabled -PathType Leaf) { $aliasCandidates += @($aliasDisabled) }
  if ($aliasCandidates.Count -gt 1) {
    Add-Failure -Failures $failures -Message ("Both virtio-input.inf and virtio-input.inf.disabled exist in {0}; only one should exist." -f $infDir)
  }
  elseif ($aliasCandidates.Count -eq 1 -and (Test-Path -LiteralPath $canonicalInf -PathType Leaf)) {
    try {
      $canonicalBytes = Inf-BytesFromFirstSection -Path $canonicalInf
      $aliasBytes = Inf-BytesFromFirstSection -Path $aliasCandidates[0]
      # virtio-input legacy alias policy permits controlled divergence in the models sections.
      $canonicalBytes = Strip-InfSectionsBytes -Data $canonicalBytes -DropSections @('Aero.NTx86', 'Aero.NTamd64')
      $aliasBytes = Strip-InfSectionsBytes -Data $aliasBytes -DropSections @('Aero.NTx86', 'Aero.NTamd64')

      $equal = $true
      if ($canonicalBytes.Length -ne $aliasBytes.Length) {
        $equal = $false
      }
      else {
        for ($i = 0; $i -lt $canonicalBytes.Length; $i++) {
          if ($canonicalBytes[$i] -ne $aliasBytes[$i]) {
            $equal = $false
            break
          }
        }
      }

      if (-not $equal) {
        Add-Failure -Failures $failures -Message ("virtio-input INF alias drift detected: {0} vs {1}. Outside the models sections ([Aero.NTx86] / [Aero.NTamd64]), from the first section header ([Version]) onward, the alias must be byte-for-byte identical to the canonical INF (only the leading banner/comments may differ). Tip: run python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py" -f $canonicalInf, $aliasCandidates[0])
      }
    }
    catch {
      Add-Failure -Failures $failures -Message ("Unable to validate virtio-input INF alias drift: {0}" -f $_.Exception.Message)
    }
  }

#------------------------------------------------------------------------------
# Version section basics
#------------------------------------------------------------------------------
$expectedClassLine = 'Class = HIDClass'
if (-not $sections.ContainsKey('Version')) {
  Add-Failure -Failures $failures -Message "Missing required section [Version]."
}
$versionLines = if ($sections.ContainsKey('Version')) { $sections['Version'] } else { $lines }

if ((Get-MatchingLines -Lines $versionLines -Regex '(?i)^Class\s*=\s*HIDClass$').Count -eq 0) {
  $found = Get-MatchingLines -Lines $versionLines -Regex '(?i)^Class\s*='
  if ($found.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing '{0}'. Ensure [Version] sets Class = HIDClass." -f $expectedClassLine)
  }
  else {
    Add-Failure -Failures $failures -Message ("Expected '{0}', but found: {1}" -f $expectedClassLine, ($found -join '; '))
  }
}

$expectedCatalogLine = 'CatalogFile = aero_virtio_input.cat'
if ((Get-MatchingLines -Lines $versionLines -Regex '(?i)^CatalogFile\s*=\s*aero_virtio_input\.cat$').Count -eq 0) {
  $found = Get-MatchingLines -Lines $versionLines -Regex '(?i)^CatalogFile\s*='
  if ($found.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing '{0}'. The package is expected to ship a catalog named aero_virtio_input.cat." -f $expectedCatalogLine)
  }
  else {
    Add-Failure -Failures $failures -Message ("Expected '{0}', but found: {1}" -f $expectedCatalogLine, ($found -join '; '))
  }
}

#------------------------------------------------------------------------------
# KMDF targeting
#------------------------------------------------------------------------------
$expectedKmdfLine = 'KmdfLibraryVersion = 1.9'
$installWdfSections = @('AeroVirtioInput_Install.NTx86.Wdf', 'AeroVirtioInput_Install.NTamd64.Wdf')
foreach ($installSect in $installWdfSections) {
  if (-not $sections.ContainsKey($installSect)) {
    Add-Failure -Failures $failures -Message ("Missing required section [{0}] (KMDF install section)." -f $installSect)
    continue
  }

  $installLines = $sections[$installSect]
  $kmdfServiceLines = Get-MatchingLines -Lines $installLines -Regex '(?i)^KmdfService\s*='
  if ($kmdfServiceLines.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing KmdfService directive in [{0}]." -f $installSect)
    continue
  }

  $wdfSectNames = @()
  foreach ($line in $kmdfServiceLines) {
    if ($line -match '(?i)^KmdfService\s*=\s*[^,]+\s*,\s*(?<sect>[^,\s]+)\s*$') {
      $wdfSectNames += $Matches['sect'].Trim()
    }
    else {
      Add-Failure -Failures $failures -Message ("Unable to parse KmdfService line in [{0}]: {1}" -f $installSect, $line)
    }
  }
  $wdfSectNames = @($wdfSectNames | Where-Object { $_.Length -gt 0 } | Select-Object -Unique)
  if ($wdfSectNames.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("No KMDF Wdf section names could be parsed from [{0}]." -f $installSect)
    continue
  }

  foreach ($wdfSect in $wdfSectNames) {
    if (-not $sections.ContainsKey($wdfSect)) {
      Add-Failure -Failures $failures -Message ("KMDF Wdf section referenced by [{0}] does not exist: [{1}]." -f $installSect, $wdfSect)
      continue
    }

    $wdfLines = $sections[$wdfSect]
    if ((Get-MatchingLines -Lines $wdfLines -Regex '(?i)^KmdfLibraryVersion\s*=\s*1\.9$').Count -eq 0) {
      $found = Get-MatchingLines -Lines $wdfLines -Regex '(?i)^KmdfLibraryVersion\s*='
      if ($found.Count -eq 0) {
        Add-Failure -Failures $failures -Message ("Missing '{0}' in [{1}]. Windows 7 SP1 includes KMDF 1.9 in-box and the INF must declare it." -f $expectedKmdfLine, $wdfSect)
      }
      else {
        Add-Failure -Failures $failures -Message ("Expected '{0}' in [{1}], but found: {2}" -f $expectedKmdfLine, $wdfSect, ($found -join '; '))
      }
    }
  }
}

#------------------------------------------------------------------------------
# Hardware IDs (Aero contract v1)
#------------------------------------------------------------------------------
# Hardware ID policy:
# - Canonical keyboard/mouse INF (`aero_virtio_input.inf`) is SUBSYS-only: it must include only the Aero keyboard/mouse
#   subsystem-qualified contract v1 HWIDs (distinct naming), and must NOT include the strict generic fallback.
# - Legacy alias INF (`virtio-input.inf{,.disabled}`) must include the same keyboard/mouse HWIDs and also include the
#   strict, REV-qualified generic fallback HWID (no SUBSYS): `PCI\VEN_1AF4&DEV_1052&REV_01`.
$fallbackHwid = 'PCI\VEN_1AF4&DEV_1052&REV_01'
$infBaseName = [System.IO.Path]::GetFileName($infPathResolved)
$requireFallback = ($infBaseName -ieq 'virtio-input.inf' -or $infBaseName -ieq 'virtio-input.inf.disabled')
$requiredHwids = @(
  # Aero contract v1 keyboard (SUBSYS_0010)
  'PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01',
  # Aero contract v1 mouse (SUBSYS_0011)
  'PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01'
)
if ($requireFallback) {
  # Strict generic fallback (no SUBSYS) is alias-only.
  $requiredHwids += @($fallbackHwid)
}

$modelSections = @('Aero.NTx86', 'Aero.NTamd64')
foreach ($sect in $modelSections) {
  if (-not $sections.ContainsKey($sect)) {
    Add-Failure -Failures $failures -Message ("Missing required models section [{0}]." -f $sect)
    continue
  }
  $sectLines = $sections[$sect]
  foreach ($id in $requiredHwids) {
    $regex = '(?i)' + [regex]::Escape($id)
    $matches = Get-MatchingLines -Lines $sectLines -Regex $regex
    if ($matches.Count -eq 0) {
      Add-Failure -Failures $failures -Message ("Missing required Aero contract v1 HWID in [{0}]: {1}" -f $sect, $id)
    }
    elseif ($matches.Count -ne 1) {
      Add-Failure -Failures $failures -Message ("Expected exactly one model entry for HWID in [{0}] ({1}), but found {2}: {3}" -f $sect, $id, $matches.Count, ($matches -join '; '))
    }
  }

  if (-not $requireFallback) {
    $regex = '(?i)' + [regex]::Escape($fallbackHwid)
    $matches = Get-MatchingLines -Lines $sectLines -Regex $regex
    if ($matches.Count -ne 0) {
      Add-Failure -Failures $failures -Message ("Unexpected strict generic fallback HWID in canonical INF models section [{0}]: {1}. Fallback binding is alias-only." -f $sect, $fallbackHwid)
    }
  }
}

#------------------------------------------------------------------------------
# Device descriptions (distinct keyboard vs mouse in Device Manager)
#------------------------------------------------------------------------------
# The INF is expected to bind both PCI functions (keyboard + mouse) to the same
# install sections, but with distinct DeviceDesc strings so they appear as
# separate named devices in Device Manager.
$requiredModelMappings = @(
  @{
    Name = 'NTx86 keyboard mapping'
    Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioKeyboard.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTx86') + '\s*,\s*' + [regex]::Escape('PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01') + '$')
    Message = 'Missing x86 keyboard model line (expected %AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, ...SUBSYS_00101AF4... ).'
  },
  @{
    Name = 'NTx86 mouse mapping'
    Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioMouse.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTx86') + '\s*,\s*' + [regex]::Escape('PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01') + '$')
    Message = 'Missing x86 mouse model line (expected %AeroVirtioMouse.DeviceDesc% = AeroVirtioInput_Install.NTx86, ...SUBSYS_00111AF4... ).'
  },
  @{
    Name = 'NTamd64 keyboard mapping'
    Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioKeyboard.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTamd64') + '\s*,\s*' + [regex]::Escape('PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01') + '$')
    Message = 'Missing x64 keyboard model line (expected %AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, ...SUBSYS_00101AF4... ).'
  },
  @{
    Name = 'NTamd64 mouse mapping'
    Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioMouse.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTamd64') + '\s*,\s*' + [regex]::Escape('PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01') + '$')
    Message = 'Missing x64 mouse model line (expected %AeroVirtioMouse.DeviceDesc% = AeroVirtioInput_Install.NTamd64, ...SUBSYS_00111AF4... ).'
  }
 )

if ($requireFallback) {
  $requiredModelMappings += @(
    @{
      Name = 'NTx86 fallback mapping'
      Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioInput.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTx86') + '\s*,\s*' + [regex]::Escape($fallbackHwid) + '$')
      Message = 'Missing x86 fallback model line (expected %AeroVirtioInput.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\\VEN_1AF4&DEV_1052&REV_01).'
    },
    @{
      Name = 'NTamd64 fallback mapping'
      Regex = ('(?i)^' + [regex]::Escape('%AeroVirtioInput.DeviceDesc%') + '\s*=\s*' + [regex]::Escape('AeroVirtioInput_Install.NTamd64') + '\s*,\s*' + [regex]::Escape($fallbackHwid) + '$')
      Message = 'Missing x64 fallback model line (expected %AeroVirtioInput.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\\VEN_1AF4&DEV_1052&REV_01).'
    }
  )
}

foreach ($m in $requiredModelMappings) {
  if ((Get-MatchingLines -Lines $lines -Regex $m.Regex).Count -eq 0) {
    Add-Failure -Failures $failures -Message $m.Message
  }
}

# Enforce revision gating for all virtio-input HWIDs in the PCI\VEN_1AF4&DEV_1052 family.
$baseHwid = 'PCI\VEN_1AF4&DEV_1052'
$expectedRevTag = '&REV_01'
foreach ($sect in $modelSections) {
  if (-not $sections.ContainsKey($sect)) { continue }
  foreach ($line in $sections[$sect]) {
    $parts = Split-InfCommaList -Text $line
    foreach ($part in $parts) {
      $p = $part.Trim()
      if ($p.ToLower().StartsWith($baseHwid.ToLower())) {
        if ($p.ToLower() -notmatch [regex]::Escape($expectedRevTag.ToLower())) {
          Add-Failure -Failures $failures -Message (("Unexpected virtio-input HWID without required revision gating in [{0}]: {1}. " +
            "Expected all model entries in the {2} family to include {3}.") -f $sect, $p, $baseHwid, $expectedRevTag)
        }
      }
    }
  }
}

# Disallow tablet subsystem IDs in the keyboard/mouse INF to keep bindings disjoint.
$forbiddenTabletSubsysRegex = '(?i)' + [regex]::Escape('SUBSYS_00121AF4')
foreach ($sect in $modelSections) {
  if (-not $sections.ContainsKey($sect)) { continue }
  if ((Get-MatchingLines -Lines $sections[$sect] -Regex $forbiddenTabletSubsysRegex).Count -ne 0) {
    Add-Failure -Failures $failures -Message (("Unexpected tablet subsystem ID in [{0}] (SUBSYS_00121AF4). " +
      "Tablet devices should bind via aero_virtio_tablet.inf, not aero_virtio_input.inf.") -f $sect)
  }
}
$requiredStrings = @(
  @{ Name = 'AeroVirtioKeyboard.DeviceDesc'; Regex = '(?i)^AeroVirtioKeyboard\.DeviceDesc\s*=\s*".*"$' },
  @{ Name = 'AeroVirtioMouse.DeviceDesc';    Regex = '(?i)^AeroVirtioMouse\.DeviceDesc\s*=\s*".*"$' },
  @{ Name = 'AeroVirtioInput.DeviceDesc';    Regex = '(?i)^AeroVirtioInput\.DeviceDesc\s*=\s*".*"$' }
)

foreach ($s in $requiredStrings) {
  if ((Get-MatchingLines -Lines $lines -Regex $s.Regex).Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing required [Strings] entry: {0}" -f $s.Name)
  }
}

if ($sections.ContainsKey('Strings')) {
  $stringsLines = $sections['Strings']

  $kbdLines = Get-MatchingLines -Lines $stringsLines -Regex '(?i)^AeroVirtioKeyboard\.DeviceDesc\s*='
  $mouseLines = Get-MatchingLines -Lines $stringsLines -Regex '(?i)^AeroVirtioMouse\.DeviceDesc\s*='
  $genericLines = Get-MatchingLines -Lines $stringsLines -Regex '(?i)^AeroVirtioInput\.DeviceDesc\s*='

  if ($kbdLines.Count -ge 1 -and $mouseLines.Count -ge 1) {
    $kbdVal = $null
    $mouseVal = $null
    $genericVal = $null

    if ($kbdLines[0] -match '(?i)^AeroVirtioKeyboard\.DeviceDesc\s*=\s*(?<val>.+)$') {
      $kbdVal = Unquote-InfString -Text $Matches['val']
    }
    if ($mouseLines[0] -match '(?i)^AeroVirtioMouse\.DeviceDesc\s*=\s*(?<val>.+)$') {
      $mouseVal = Unquote-InfString -Text $Matches['val']
    }
    if ($genericLines.Count -ge 1 -and ($genericLines[0] -match '(?i)^AeroVirtioInput\.DeviceDesc\s*=\s*(?<val>.+)$')) {
      $genericVal = Unquote-InfString -Text $Matches['val']
    }

    if (($null -ne $kbdVal) -and ($null -ne $mouseVal)) {
      if ($kbdVal -eq $mouseVal) {
        Add-Failure -Failures $failures -Message ("Keyboard and mouse DeviceDesc strings are identical ('{0}'); they should be distinct so the devices can be distinguished in Device Manager." -f $kbdVal)
      }
    }

    if (($null -ne $genericVal) -and ($null -ne $kbdVal) -and ($genericVal -eq $kbdVal)) {
      Add-Failure -Failures $failures -Message ("Generic DeviceDesc string AeroVirtioInput.DeviceDesc must not equal AeroVirtioKeyboard.DeviceDesc ('{0}')." -f $genericVal)
    }
    if (($null -ne $genericVal) -and ($null -ne $mouseVal) -and ($genericVal -eq $mouseVal)) {
      Add-Failure -Failures $failures -Message ("Generic DeviceDesc string AeroVirtioInput.DeviceDesc must not equal AeroVirtioMouse.DeviceDesc ('{0}')." -f $genericVal)
    }
  }
}

#------------------------------------------------------------------------------
# Interrupt Management (MSI/MSI-X)
#------------------------------------------------------------------------------
$installHwSections = @('AeroVirtioInput_Install.NTx86.HW', 'AeroVirtioInput_Install.NTamd64.HW')
foreach ($installSect in $installHwSections) {
  if (-not $sections.ContainsKey($installSect)) {
    Add-Failure -Failures $failures -Message ("Missing required section [{0}] (HW install section)." -f $installSect)
    continue
  }

  $installLines = $sections[$installSect]
  $addRegLines = Get-MatchingLines -Lines $installLines -Regex '(?i)^AddReg\s*='
  if ($addRegLines.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing AddReg directive in [{0}] (MSI settings will not be applied)." -f $installSect)
    continue
  }

  $addRegSections = @()
  foreach ($line in $addRegLines) {
    if ($line -match '(?i)^AddReg\s*=\s*(?<list>.+)$') {
      $addRegSections += Split-InfCommaList -Text $Matches['list']
    }
    else {
      Add-Failure -Failures $failures -Message ("Unable to parse AddReg line in [{0}]: {1}" -f $installSect, $line)
    }
  }
  $addRegSections = @($addRegSections | Where-Object { $_.Length -gt 0 } | Select-Object -Unique)
  if ($addRegSections.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("No AddReg section names could be parsed from [{0}]." -f $installSect)
    continue
  }

  $msiLines = New-Object System.Collections.Generic.List[string]
  foreach ($regSect in $addRegSections) {
    if (-not $sections.ContainsKey($regSect)) {
      Add-Failure -Failures $failures -Message ("AddReg section referenced by [{0}] does not exist: [{1}]." -f $installSect, $regSect)
      continue
    }
    foreach ($l in $sections[$regSect]) {
      $msiLines.Add($l)
    }
  }

  # Key creation: HKR, "Interrupt Management",,0x00000010
  $imKeyLines = Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management"\s*,\s*,'
  if ($imKeyLines.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing MSI registry key creation in AddReg sections referenced by [{0}] (expected HKR, \"Interrupt Management\",,0x00000010)." -f $installSect)
  }
  else {
    $parsed = $false
    foreach ($line in $imKeyLines) {
      if ($line -match '(?i)^HKR\s*,\s*"Interrupt Management"\s*,\s*,\s*(?<flags>[^,\s]+)\s*$') {
        $parsed = $true
        try {
          $flags = Parse-InfInteger -Text $Matches['flags']
          if (($flags -band 0x10) -eq 0) {
            Add-Failure -Failures $failures -Message ("Interrupt Management key line does not include 0x10 (key-only) flag (referenced by [{0}]). Line: {1}" -f $installSect, $line)
          }
        }
        catch {
          Add-Failure -Failures $failures -Message ("Unable to parse Interrupt Management key flags (referenced by [{0}]). Line: {1}" -f $installSect, $line)
        }
      }
    }
    if (-not $parsed) {
      Add-Failure -Failures $failures -Message ("Unable to parse Interrupt Management key line(s) (referenced by [{0}]): {1}" -f $installSect, ($imKeyLines -join '; '))
    }
  }

  # Enable MSI: HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
  $msiSupportedLines = Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MSISupported\s*,'
  if ($msiSupportedLines.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing MSI enablement in AddReg sections referenced by [{0}] (expected HKR, \"Interrupt Management\\MessageSignaledInterruptProperties\", MSISupported, ..., 1)." -f $installSect)
  }
  else {
    $parsed = $false
    foreach ($line in $msiSupportedLines) {
      if ($line -match '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MSISupported\s*,\s*[^,]*\s*,\s*(?<val>[^,\s]+)\s*$') {
        $parsed = $true
        try {
          $val = Parse-InfInteger -Text $Matches['val']
          if ($val -ne 1) {
            Add-Failure -Failures $failures -Message ("MSISupported is {0}, expected 1 (enabled) (referenced by [{1}]). Line: {2}" -f $val, $installSect, $line)
          }
        }
        catch {
          Add-Failure -Failures $failures -Message ("Unable to parse MSISupported value (referenced by [{0}]). Line: {1}" -f $installSect, $line)
        }
      }
    }
    if (-not $parsed) {
      Add-Failure -Failures $failures -Message ("Unable to parse MSISupported line(s) (referenced by [{0}]): {1}" -f $installSect, ($msiSupportedLines -join '; '))
    }
  }

  # Request enough messages (>= 3): HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, <n>
  $msgLines = Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MessageNumberLimit\s*,'
  if ($msgLines.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing MSI message count request in AddReg sections referenced by [{0}] (expected HKR, \"Interrupt Management\\MessageSignaledInterruptProperties\", MessageNumberLimit, ..., <n>)." -f $installSect)
  }
  else {
    $min = 3
    $ok = $false
    $bestVal = $null

    foreach ($line in $msgLines) {
      if ($line -match '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MessageNumberLimit\s*,\s*[^,]*\s*,\s*(?<val>[^,\s]+)\s*$') {
        try {
          $val = Parse-InfInteger -Text $Matches['val']
          $bestVal = $val
          if ($val -ge $min) {
            $ok = $true
            break
          }
        }
        catch {
          Add-Failure -Failures $failures -Message ("Unable to parse MessageNumberLimit value (referenced by [{0}]). Line: {1}" -f $installSect, $line)
        }
      }
      else {
        Add-Failure -Failures $failures -Message ("Unable to parse MessageNumberLimit line (referenced by [{0}]). Line: {1}" -f $installSect, $line)
      }
    }

    if (-not $ok) {
      if ($bestVal -ne $null) {
        Add-Failure -Failures $failures -Message ("MessageNumberLimit is {0} in AddReg sections referenced by [{1}], but Aero requires >= {2} (config + at least two queues)." -f $bestVal, $installSect, $min)
      }
      else {
        Add-Failure -Failures $failures -Message ("MessageNumberLimit is missing/invalid in AddReg sections referenced by [{0}]; Aero requires >= {1} (config + at least two queues)." -f $installSect, $min)
      }
    }
  }
}

  if ($failures.Count -gt 0) {
    Write-Host ("INF validation FAILED: {0}" -f $infPathResolved)
    Write-Host ""
    foreach ($f in $failures) {
      Write-Host ("  - {0}" -f $f)
    }
    Write-Host ""
    $exitCode = 1
  }
  else {
    Write-Host ("INF validation OK: {0}" -f $infPathResolved)
  }
}
catch {
  $exitCode = 1
  Write-Error $_.Exception.Message
}

exit $exitCode
