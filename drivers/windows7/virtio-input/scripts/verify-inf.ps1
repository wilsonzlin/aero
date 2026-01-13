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
  - Must include the contract v1 HWID set (revision gated, REV_01)
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

function Get-MatchingLines([string[]]$Lines, [string]$Regex) {
  return @($Lines | Where-Object { $_ -match $Regex })
}

function Add-Failure([System.Collections.Generic.List[string]]$Failures, [string]$Message) {
  [void]$Failures.Add($Message)
}

$infPathResolved = Resolve-ExistingFile -Path $InfPath -ArgName '-InfPath'

$rawLines = Get-Content -LiteralPath $infPathResolved -ErrorAction Stop
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
if (-not $sections.ContainsKey('AeroVirtioInput_WdfSect')) {
  Add-Failure -Failures $failures -Message "Missing required section [AeroVirtioInput_WdfSect] (KMDF settings)."
}
$wdfLines = if ($sections.ContainsKey('AeroVirtioInput_WdfSect')) { $sections['AeroVirtioInput_WdfSect'] } else { $lines }

if ((Get-MatchingLines -Lines $wdfLines -Regex '(?i)^KmdfLibraryVersion\s*=\s*1\.9$').Count -eq 0) {
  $found = Get-MatchingLines -Lines $wdfLines -Regex '(?i)^KmdfLibraryVersion\s*='
  if ($found.Count -eq 0) {
    Add-Failure -Failures $failures -Message ("Missing '{0}'. Windows 7 SP1 includes KMDF 1.9 in-box and the INF must declare it." -f $expectedKmdfLine)
  }
  else {
    Add-Failure -Failures $failures -Message ("Expected '{0}', but found: {1}" -f $expectedKmdfLine, ($found -join '; '))
  }
}

#------------------------------------------------------------------------------
# Hardware IDs (Aero contract v1)
#------------------------------------------------------------------------------
$requiredHwids = @(
  'PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01',
  'PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01',
  'PCI\VEN_1AF4&DEV_1052&REV_01'
)

$modelSections = @('Aero.NTx86', 'Aero.NTamd64')
foreach ($sect in $modelSections) {
  if (-not $sections.ContainsKey($sect)) {
    Add-Failure -Failures $failures -Message ("Missing required models section [{0}]." -f $sect)
    continue
  }
  $sectLines = $sections[$sect]
  foreach ($id in $requiredHwids) {
    $regex = '(?i)' + [regex]::Escape($id)
    if ((Get-MatchingLines -Lines $sectLines -Regex $regex).Count -eq 0) {
      Add-Failure -Failures $failures -Message ("Missing required Aero contract v1 HWID in [{0}]: {1}" -f $sect, $id)
    }
  }
}

#------------------------------------------------------------------------------
# Interrupt Management (MSI/MSI-X)
#------------------------------------------------------------------------------
# Verify the AddReg section contents (ensures removal of the section or one line fails fast).
$msiSectionName = 'AeroVirtioInput_InterruptManagement_AddReg'
if (-not $sections.ContainsKey($msiSectionName)) {
  Add-Failure -Failures $failures -Message ("Missing required section [{0}] (MSI/MSI-X AddReg entries)." -f $msiSectionName)
}
$msiLines = if ($sections.ContainsKey($msiSectionName)) { $sections[$msiSectionName] } else { $lines }

# Key creation: HKR, "Interrupt Management",,0x00000010
if ((Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management"\s*,\s*,').Count -eq 0) {
  Add-Failure -Failures $failures -Message 'Missing MSI registry key creation (expected HKR, "Interrupt Management",,0x00000010).'
}

# Enable MSI: HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
$msiSupportedLines = Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MSISupported\s*,'
if ($msiSupportedLines.Count -eq 0) {
  Add-Failure -Failures $failures -Message 'Missing MSI enablement (expected HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, ..., 1).'
}
else {
  $parsed = $false
  foreach ($line in $msiSupportedLines) {
    if ($line -match '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MSISupported\s*,\s*[^,]*\s*,\s*(?<val>[^,\s]+)\s*$') {
      $parsed = $true
      try {
        $val = Parse-InfInteger -Text $Matches['val']
        if ($val -ne 1) {
          Add-Failure -Failures $failures -Message ("MSISupported is {0}, expected 1 (enabled). Line: {1}" -f $val, $line)
        }
      }
      catch {
        Add-Failure -Failures $failures -Message ("Unable to parse MSISupported value in line: {0}" -f $line)
      }
    }
  }
  if (-not $parsed) {
    Add-Failure -Failures $failures -Message ("Unable to parse MSISupported line(s): {0}" -f ($msiSupportedLines -join '; '))
  }
}

# Request enough messages (>= 3): HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, <n>
$msgLines = Get-MatchingLines -Lines $msiLines -Regex '(?i)^HKR\s*,\s*"Interrupt Management\\\\MessageSignaledInterruptProperties"\s*,\s*MessageNumberLimit\s*,'
if ($msgLines.Count -eq 0) {
  Add-Failure -Failures $failures -Message 'Missing MSI message count request (expected HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, ..., <n>).'
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
        Add-Failure -Failures $failures -Message ("Unable to parse MessageNumberLimit value in line: {0}" -f $line)
      }
    }
    else {
      Add-Failure -Failures $failures -Message ("Unable to parse MessageNumberLimit line: {0}" -f $line)
    }
  }

  if (-not $ok) {
    if ($bestVal -ne $null) {
      Add-Failure -Failures $failures -Message ("MessageNumberLimit is {0}, but Aero requires >= {1} (config + at least two queues)." -f $bestVal, $min)
    }
    else {
      Add-Failure -Failures $failures -Message ("MessageNumberLimit is missing/invalid; Aero requires >= {0} (config + at least two queues)." -f $min)
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
  exit 1
}

Write-Host ("INF validation OK: {0}" -f $infPathResolved)
exit 0
