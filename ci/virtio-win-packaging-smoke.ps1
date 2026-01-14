[CmdletBinding()]
param(
  [string]$OutRoot = (Join-Path $PSScriptRoot "..\out\virtio-win-packaging-smoke"),
  [switch]$OmitOptionalDrivers,
  [string]$GuestToolsSpecPath,
  # Controls the wrapper's extraction defaults (-Profile). When set to "auto" (default),
  # pick a profile that matches the well-known in-repo spec filenames.
  [ValidateSet("auto", "minimal", "full")]
  [string]$GuestToolsProfile = "auto",
  # Also exercise the ISO-mounting code paths by creating a synthetic virtio-win ISO
  # (via ci/lib/New-IsoFile.ps1; deterministic via the Rust ISO writer, requires cargo)
  # and running make-driver-pack.ps1 with -VirtioWinIso.
  [switch]$TestIsoMode,
  # Skip the second Guest Tools packaging run that validates wrapper defaults.
  # Useful for reducing CI time when the defaults check is already covered by another job.
  [switch]$SkipGuestToolsDefaultsCheck,
  # Also validate the Guest Tools packaging path with a non-default signing policy (`test`),
  # ensuring certificate inclusion/requirements are enforced correctly.
  [switch]$TestSigningPolicies
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}

function Assert-SafeOutRoot {
  param(
    [Parameter(Mandatory = $true)][string]$RepoRoot,
    [Parameter(Mandatory = $true)][string]$OutRoot
  )

  $repoFull = [System.IO.Path]::GetFullPath($RepoRoot)
  $outFull = [System.IO.Path]::GetFullPath($OutRoot)
  $driveRoot = [System.IO.Path]::GetPathRoot($outFull)

  if ($outFull -eq $repoFull) {
    throw "Refusing to use -OutRoot at the repo root (would delete the working tree): $outFull"
  }
  if ($outFull -eq $driveRoot) {
    throw "Refusing to use -OutRoot at the drive root: $outFull"
  }

  $repoOut = [System.IO.Path]::GetFullPath((Join-Path $repoFull "out"))
  $repoOutPrefix = $repoOut.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
  if ($outFull.Equals($repoOut, [System.StringComparison]::OrdinalIgnoreCase) -or $outFull.StartsWith($repoOutPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    return
  }

  if ($outFull -match '(?i)virtio-win-packaging-smoke') {
    return
  }

  throw "Refusing to use -OutRoot outside '$repoOut' unless the path contains 'virtio-win-packaging-smoke': $outFull"
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Recurse -Force
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Ensure-Directory {
  param([Parameter(Mandatory = $true)][string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    New-Item -ItemType Directory -Force -Path $Path | Out-Null
  }
}

function Resolve-Python {
  $candidates = @("python3", "python", "py")
  foreach ($c in $candidates) {
    $cmd = Get-Command $c -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
  }
  return $null
}

function Resolve-WindowsPowerShell {
  $cmd = Get-Command "powershell" -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  return $null
}

function Write-SyntheticInf {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$BaseName,
    [string]$HardwareId,
    [switch]$AddServiceInlineComment
  )

  $lines = New-Object "System.Collections.Generic.List[string]"
  $lines.Add("; Synthetic INF for CI virtio-win packaging smoke tests.") | Out-Null
  $lines.Add("[Version]") | Out-Null
  $lines.Add('Signature="$WINDOWS NT$"') | Out-Null
  $lines.Add("Class=System") | Out-Null
  $lines.Add("Provider=%ProviderName%") | Out-Null
  $lines.Add("") | Out-Null
  $lines.Add("[SourceDisksFiles]") | Out-Null
  $lines.Add("$BaseName.sys=1") | Out-Null
  $lines.Add("$BaseName.cat=1") | Out-Null

  if ($HardwareId) {
    $lines.Add("") | Out-Null
    $lines.Add("[HardwareIds]") | Out-Null
    $lines.Add($HardwareId) | Out-Null
  }

  # `guest-tools/setup.cmd` requires that config/devices.cmd service names match the INF AddService
  # names for virtio-blk and other drivers. Include an explicit AddService directive so the smoke
  # test can validate the packaged devices.cmd stays in sync.
  $lines.Add("") | Out-Null
  $lines.Add("[DefaultInstall.Services]") | Out-Null
  if ($AddServiceInlineComment) {
    # Regression: some INFs contain an inline comment immediately after the service token, which
    # historically caused Guest Tools contract auto-detection to pick a service name with a
    # trailing ';' (e.g. "viostor;").
    $lines.Add("AddService = $BaseName; comment, 0x00000002, ${BaseName}_Service_Inst") | Out-Null
  } else {
    $lines.Add("AddService = $BaseName, 0x00000002, ${BaseName}_Service_Inst") | Out-Null
  }
  $lines.Add("") | Out-Null
  $lines.Add("[${BaseName}_Service_Inst]") | Out-Null
  $lines.Add("ServiceType = 1") | Out-Null
  $lines.Add("StartType = 3") | Out-Null
  $lines.Add("ErrorControl = 1") | Out-Null
  $lines.Add("ServiceBinary = %12%\\$BaseName.sys") | Out-Null
  $lines.Add("") | Out-Null
  $lines.Add("[Strings]") | Out-Null
  $lines.Add('ProviderName="Aero Synthetic"') | Out-Null

  $lines | Out-File -FilePath $Path -Encoding ascii
}

function Write-SyntheticInfUtf16LeNoBom {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$BaseName,
    [string]$HardwareId,
    [switch]$AddServiceInlineComment
  )

  $tmp = [System.IO.Path]::GetTempFileName()
  try {
    Write-SyntheticInf -Path $tmp -BaseName $BaseName -HardwareId $HardwareId -AddServiceInlineComment:$AddServiceInlineComment
    $content = Get-Content -LiteralPath $tmp -Raw
  } finally {
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
  }

  # UTF-16LE without a BOM is a common INF encoding in the wild. Use this encoding to ensure
  # ci/package-guest-tools.ps1's contract auto-patching (HWID matching + AddService scanning)
  # does not depend on BOM-based detection.
  $enc = New-Object System.Text.UnicodeEncoding($false, $false) # little-endian, no BOM
  [System.IO.File]::WriteAllText($Path, $content, $enc)
}

function Write-PlaceholderBinary {
  param([Parameter(Mandatory = $true)][string]$Path)
  "placeholder" | Out-File -FilePath $Path -Encoding ascii
}

function New-SyntheticDriverFiles {
  param(
    [Parameter(Mandatory = $true)][string]$VirtioRoot,
    [Parameter(Mandatory = $true)][string]$UpstreamDirName,
    [Parameter(Mandatory = $true)][string]$InfBaseName,
    [Parameter(Mandatory = $true)][string]$OsDirName,
    [Parameter(Mandatory = $true)][string]$ArchDirName,
    [string]$HardwareId,
    [switch]$AddServiceInlineComment,
    # When set, write the INF as UTF-16LE without a BOM (common in the wild).
    [switch]$InfUtf16LeNoBom
  )

  $dir = Join-Path $VirtioRoot (Join-Path $UpstreamDirName (Join-Path $OsDirName $ArchDirName))
  Ensure-Directory -Path $dir

  $infName = "$InfBaseName.inf"
  $sysName = "$InfBaseName.sys"
  $catName = "$InfBaseName.cat"

  $infPath = Join-Path $dir $infName
  if ($InfUtf16LeNoBom) {
    Write-SyntheticInfUtf16LeNoBom -Path $infPath -BaseName $InfBaseName -HardwareId $HardwareId -AddServiceInlineComment:$AddServiceInlineComment
  } else {
    Write-SyntheticInf -Path $infPath -BaseName $InfBaseName -HardwareId $HardwareId -AddServiceInlineComment:$AddServiceInlineComment
  }

  Write-PlaceholderBinary -Path (Join-Path $dir $sysName)
  Write-PlaceholderBinary -Path (Join-Path $dir $catName)
}

function Get-SpecDriverNames {
  param([Parameter(Mandatory = $true)][string]$SpecPath)

  $specObj = Get-Content -LiteralPath $SpecPath -Raw | ConvertFrom-Json
  $specDriverNames = @()
  if ($null -ne $specObj.drivers) {
    $specDriverNames += @($specObj.drivers | ForEach-Object { $_.name })
  }
  if ($null -ne $specObj.required_drivers) {
    $specDriverNames += @($specObj.required_drivers | ForEach-Object { $_.name })
  }
  return @(
    $specDriverNames |
      Where-Object { $_ } |
      ForEach-Object { $_.ToString().ToLowerInvariant() } |
      Sort-Object -Unique
  )
}

function Convert-TextBytesWithEncodingDetection {
  param([Parameter(Mandatory = $true)][byte[]]$Bytes)

  if ($null -eq $Bytes -or $Bytes.Length -eq 0) {
    return ""
  }

  $offset = 0
  $encoding = $null

  if ($Bytes.Length -ge 3 -and $Bytes[0] -eq 0xEF -and $Bytes[1] -eq 0xBB -and $Bytes[2] -eq 0xBF) {
    $encoding = [System.Text.Encoding]::UTF8
    $offset = 3
  } elseif ($Bytes.Length -ge 2 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE) {
    $encoding = [System.Text.Encoding]::Unicode # UTF-16LE
    $offset = 2
  } elseif ($Bytes.Length -ge 2 -and $Bytes[0] -eq 0xFE -and $Bytes[1] -eq 0xFF) {
    $encoding = [System.Text.Encoding]::BigEndianUnicode # UTF-16BE
    $offset = 2
  } elseif (($Bytes.Length % 2) -eq 0 -and $Bytes.Length -ge 4) {
    # Heuristic for BOM-less UTF-16. INF files are typically ASCII-ish, so UTF-16 text tends
    # to have a high number of 0x00 bytes in either even or odd positions.
    $pairs = [int]($Bytes.Length / 2)
    $nulEven = 0
    $nulOdd = 0
    for ($i = 0; $i -lt $Bytes.Length; $i += 2) {
      if ($Bytes[$i] -eq 0) { $nulEven += 1 }
    }
    for ($i = 1; $i -lt $Bytes.Length; $i += 2) {
      if ($Bytes[$i] -eq 0) { $nulOdd += 1 }
    }

    $nulRatio = ($nulEven + $nulOdd) / [double]$Bytes.Length
    $evenRatio = $nulEven / [double]$pairs
    $oddRatio = $nulOdd / [double]$pairs

    if ($nulRatio -ge 0.2 -and ([Math]::Max($evenRatio, $oddRatio) -ge 0.5)) {
      # If odd bytes are mostly NULs, that's typical UTF-16LE. If even bytes are mostly NULs,
      # that's typical UTF-16BE.
      $encoding = if ($oddRatio -ge $evenRatio) { [System.Text.Encoding]::Unicode } else { [System.Text.Encoding]::BigEndianUnicode }
      $offset = 0
    }
  }

  if (-not $encoding) {
    $encoding = [System.Text.Encoding]::UTF8
    $offset = 0
  }

  $text = $encoding.GetString($Bytes, $offset, ($Bytes.Length - $offset))

  # Strip a leading BOM codepoint (defensive), and strip NULs to handle BOM-less UTF-16 that
  # was decoded using an ASCII-compatible fallback.
  if ($text.Length -gt 0 -and $text[0] -eq [char]0xFEFF) {
    $text = $text.Substring(1)
  }
  if ($text.IndexOf([char]0) -ge 0) {
    $text = $text.Replace([char]0, "")
  }

  return $text
}

function Try-ReadZipEntryText {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$EntryPath
  )

  Add-Type -AssemblyName System.IO.Compression
  $fs = [System.IO.File]::OpenRead($ZipPath)
  $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
  try {
    $entry = $zip.GetEntry($EntryPath)
    if (-not $entry) {
      return $null
    }
    $es = $entry.Open()
    try {
      $ms = New-Object System.IO.MemoryStream
      try {
        $es.CopyTo($ms)
        return Convert-TextBytesWithEncodingDetection -Bytes $ms.ToArray()
      } finally {
        $ms.Dispose()
      }
    } finally {
      $es.Dispose()
    }
  } finally {
    $zip.Dispose()
    $fs.Dispose()
  }
}

function ReadZipEntryText {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$EntryPath
  )

  $text = Try-ReadZipEntryText -ZipPath $ZipPath -EntryPath $EntryPath
  if ($null -eq $text) {
    throw "Expected ZIP '$ZipPath' to contain entry '$EntryPath'."
  }
  return $text
}

function Test-ZipHasEntry {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$EntryPath
  )

  Add-Type -AssemblyName System.IO.Compression
  $fs = [System.IO.File]::OpenRead($ZipPath)
  $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
  try {
    return $null -ne ($zip.Entries | Where-Object { $_.FullName -eq $EntryPath } | Select-Object -First 1)
  } finally {
    $zip.Dispose()
    $fs.Dispose()
  }
}

function Assert-ZipContainsEntry {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$EntryPath
  )
  if (-not (Test-ZipHasEntry -ZipPath $ZipPath -EntryPath $EntryPath)) {
    throw "Expected ZIP '$ZipPath' to contain entry '$EntryPath'."
  }
}

function Assert-ZipNotContainsEntry {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$EntryPath
  )
  if (Test-ZipHasEntry -ZipPath $ZipPath -EntryPath $EntryPath) {
    throw "Expected ZIP '$ZipPath' to NOT contain entry '$EntryPath'."
  }
}

function Get-DevicesCmdVarValue {
  param(
    [Parameter(Mandatory = $true)][string]$DevicesCmdText,
    [Parameter(Mandatory = $true)][string]$VarName
  )

  $target = $VarName.ToUpperInvariant()
  foreach ($rawLine in ($DevicesCmdText -split "`r?`n")) {
    $line = $rawLine.Trim()
    if (-not $line) { continue }

    $lower = $line.ToLowerInvariant()
    if ($lower.StartsWith("rem") -or $lower.StartsWith("::") -or $lower.StartsWith("@echo")) {
      continue
    }

    if ($line -match '^(?i)\s*set\s+"([^=]+)=(.*)"\s*$') {
      $key = $matches[1].Trim().ToUpperInvariant()
      if ($key -eq $target) {
        return $matches[2].Trim()
      }
      continue
    }

    if ($line -match '^(?i)\s*set\s+([^=]+)=(.*)$') {
      $key = $matches[1].Trim().ToUpperInvariant()
      if ($key -eq $target) {
        return $matches[2].Trim()
      }
      continue
    }
  }

  return $null
}

function Get-DevicesCmdContractName {
  param([Parameter(Mandatory = $true)][string]$DevicesCmdText)

  foreach ($rawLine in ($DevicesCmdText -split "`r?`n")) {
    $line = $rawLine.Trim()
    if (-not $line) { continue }
    if ($line -match '^(?i)\s*rem\s+Contract name:\s*(.+?)\s*$') {
      $name = $matches[1].Trim()
      if ($name) { return $name }
    }
  }

  return $null
}

function Assert-DevicesCmdVarEquals {
  param(
    [Parameter(Mandatory = $true)][string]$DevicesCmdText,
    [Parameter(Mandatory = $true)][string]$VarName,
    [Parameter(Mandatory = $true)][string]$Expected
  )

  $actual = Get-DevicesCmdVarValue -DevicesCmdText $DevicesCmdText -VarName $VarName
  if ($null -eq $actual) {
    throw "Guest Tools devices.cmd missing expected variable: $VarName"
  }
  if ($actual.Trim().ToLowerInvariant() -ne $Expected.Trim().ToLowerInvariant()) {
    throw "Guest Tools devices.cmd $VarName mismatch: expected '$Expected', got '$actual'"
  }
}

function Set-ContractDriverServiceName {
  param(
    [Parameter(Mandatory = $true)]$Contract,
    [Parameter(Mandatory = $true)][string]$DeviceName,
    [Parameter(Mandatory = $true)][string]$ServiceName
  )

  foreach ($d in @($Contract.devices)) {
    if ($null -eq $d) { continue }
    $n = "" + $d.device
    if ($n -and ($n.ToLowerInvariant() -eq $DeviceName.ToLowerInvariant())) {
      $d.driver_service_name = $ServiceName
      return
    }
  }
  throw "Device contract is missing required device entry: $DeviceName"
}

function Get-InfAddServiceName {
  param([Parameter(Mandatory = $true)][string]$InfText)

  foreach ($rawLine in ($InfText -split "`r?`n")) {
    $line = $rawLine
    if ($line.Length -gt 0 -and $line[0] -eq [char]0xFEFF) {
      $line = $line.Substring(1)
    }

    $semi = $line.IndexOf(';')
    if ($semi -ge 0) {
      $line = $line.Substring(0, $semi)
    }
    $line = $line.Trim()
    if (-not $line) { continue }

    $m = [regex]::Match($line, '(?i)^\s*AddService\s*=\s*(.+)$')
    if (-not $m.Success) { continue }

    $rest = $m.Groups[1].Value.Trim()
    if (-not $rest) { continue }
    $rest = $rest.Replace('"', '')

    $m2 = [regex]::Match($rest, '^([^,\s]+)')
    if ($m2.Success) {
      $svc = $m2.Groups[1].Value.Trim().TrimEnd(';').Trim()
      if ($svc) { return $svc }
    }
  }
  return $null
}

function Assert-DevicesCmdServiceMatchesInf {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$DevicesCmdText,
    [Parameter(Mandatory = $true)][string]$DevicesCmdServiceVar,
    [Parameter(Mandatory = $true)][string]$InfEntryPath,
    [switch]$Optional
  )

  $expectedService = Get-DevicesCmdVarValue -DevicesCmdText $DevicesCmdText -VarName $DevicesCmdServiceVar
  if (-not $expectedService) {
    throw "Guest Tools devices.cmd missing expected service variable: $DevicesCmdServiceVar"
  }

  $infText = Try-ReadZipEntryText -ZipPath $ZipPath -EntryPath $InfEntryPath
  if (-not $infText) {
    if ($Optional) {
      return
    }
    throw "Expected Guest Tools ZIP to include INF entry: $InfEntryPath"
  }
  $infService = Get-InfAddServiceName -InfText $infText
  if (-not $infService) {
    throw "INF $InfEntryPath does not contain an AddService directive (synthetic INF regression)."
  }
  if ($infService.ToLowerInvariant() -ne $expectedService.ToLowerInvariant()) {
    throw "Service mismatch: devices.cmd $DevicesCmdServiceVar='$expectedService' but $InfEntryPath AddService='$infService'"
  }
}

function Assert-GuestToolsDevicesCmdServices {
  param(
    [Parameter(Mandatory = $true)][string]$ZipPath,
    [Parameter(Mandatory = $true)][string]$SpecPath
  )

  $devicesCmdText = ReadZipEntryText -ZipPath $ZipPath -EntryPath "config/devices.cmd"

  # Assert the packaged devices.cmd identifies the virtio-win device contract variant. We expect
  # `make-guest-tools-from-virtio-win.ps1` to pass `docs/windows-device-contract-virtio-win.json`
  # so the contract name in the header is a stable indicator of which contract generated the file
  # (in addition to the service-name assertions below).
  $repoRoot = Resolve-RepoRoot
  $contractPath = Join-Path $repoRoot "docs\windows-device-contract-virtio-win.json"
  if (-not (Test-Path -LiteralPath $contractPath -PathType Leaf)) {
    throw "Expected virtio-win Windows device contract not found: $contractPath"
  }
  $contractObj = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
  $expectedContractName = ("" + $contractObj.contract_name).Trim()
  if (-not $expectedContractName) {
    throw "virtio-win Windows device contract has empty contract_name: $contractPath"
  }
  $actualContractName = Get-DevicesCmdContractName -DevicesCmdText $devicesCmdText
  if (-not $actualContractName) {
    throw "Guest Tools devices.cmd is missing the expected 'rem Contract name: ...' header line."
  }
  if ($actualContractName.Trim().ToLowerInvariant() -ne $expectedContractName.Trim().ToLowerInvariant()) {
    throw "Guest Tools devices.cmd contract_name mismatch: expected '$expectedContractName', got '$actualContractName'"
  }

  # Required for boot-critical storage pre-seeding and network validation.
  Assert-DevicesCmdVarEquals -DevicesCmdText $devicesCmdText -VarName "AERO_VIRTIO_BLK_SERVICE" -Expected "viostor"
  Assert-DevicesCmdVarEquals -DevicesCmdText $devicesCmdText -VarName "AERO_VIRTIO_NET_SERVICE" -Expected "netkvm"

  $specDriverNames = Get-SpecDriverNames -SpecPath $SpecPath
  if ($specDriverNames -contains "vioinput") {
    Assert-DevicesCmdVarEquals -DevicesCmdText $devicesCmdText -VarName "AERO_VIRTIO_INPUT_SERVICE" -Expected "vioinput"
  }
  if ($specDriverNames -contains "viosnd") {
    Assert-DevicesCmdVarEquals -DevicesCmdText $devicesCmdText -VarName "AERO_VIRTIO_SND_SERVICE" -Expected "viosnd"
  }

  # Ensure devices.cmd service names actually match the packaged INF AddService values.
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_BLK_SERVICE" -InfEntryPath "drivers/x86/viostor/viostor.inf"
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_BLK_SERVICE" -InfEntryPath "drivers/amd64/viostor/viostor.inf"
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_NET_SERVICE" -InfEntryPath "drivers/x86/netkvm/netkvm.inf"
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_NET_SERVICE" -InfEntryPath "drivers/amd64/netkvm/netkvm.inf"
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_INPUT_SERVICE" -InfEntryPath "drivers/x86/vioinput/vioinput.inf" -Optional
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_INPUT_SERVICE" -InfEntryPath "drivers/amd64/vioinput/vioinput.inf" -Optional
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_SND_SERVICE" -InfEntryPath "drivers/x86/viosnd/viosnd.inf" -Optional
  Assert-DevicesCmdServiceMatchesInf -ZipPath $ZipPath -DevicesCmdText $devicesCmdText -DevicesCmdServiceVar "AERO_VIRTIO_SND_SERVICE" -InfEntryPath "drivers/amd64/viosnd/viosnd.inf" -Optional
}

$repoRoot = Resolve-RepoRoot

if (-not $GuestToolsSpecPath) {
  $GuestToolsSpecPath = Join-Path $repoRoot "tools\packaging\specs\win7-virtio-win.json"
} elseif (-not [System.IO.Path]::IsPathRooted($GuestToolsSpecPath)) {
  $GuestToolsSpecPath = Join-Path $repoRoot $GuestToolsSpecPath
}
$GuestToolsSpecPath = [System.IO.Path]::GetFullPath($GuestToolsSpecPath)

if (-not (Test-Path -LiteralPath $GuestToolsSpecPath -PathType Leaf)) {
  throw "Guest Tools packaging spec not found: $GuestToolsSpecPath"
}

$resolvedGuestToolsProfile = $GuestToolsProfile
if ($resolvedGuestToolsProfile -eq "auto") {
  $specBaseName = [System.IO.Path]::GetFileName($GuestToolsSpecPath).ToLowerInvariant()
  if ($specBaseName -eq "win7-virtio-win.json") {
    $resolvedGuestToolsProfile = "minimal"
  } elseif ($specBaseName -eq "win7-virtio-full.json") {
    $resolvedGuestToolsProfile = "full"
  } else {
    # Best-effort fallback: use the more inclusive profile to keep optional-driver smoke tests
    # effective even when a custom spec filename is used.
    $resolvedGuestToolsProfile = "full"
  }
}

if (-not [System.IO.Path]::IsPathRooted($OutRoot)) {
  $OutRoot = Join-Path $repoRoot $OutRoot
}
$OutRoot = [System.IO.Path]::GetFullPath($OutRoot)

Assert-SafeOutRoot -RepoRoot $repoRoot -OutRoot $OutRoot
Ensure-EmptyDirectory -Path $OutRoot

$logsDir = Join-Path $OutRoot "logs"
Ensure-Directory -Path $logsDir

$syntheticRoot = Join-Path $OutRoot "virtio-win"
Ensure-EmptyDirectory -Path $syntheticRoot

$osDir = "w7"

# Root-level license/notice files (best-effort copy). Use lowercase filenames to ensure
# packaging is robust on case-sensitive filesystems.
"license placeholder" | Out-File -FilePath (Join-Path $syntheticRoot "license.txt") -Encoding ascii
"notice placeholder" | Out-File -FilePath (Join-Path $syntheticRoot "notice.txt") -Encoding ascii
$fakeVirtioWinVersion = "0.0.0-synthetic"
$fakeVirtioWinVersion | Out-File -FilePath (Join-Path $syntheticRoot "VERSION") -Encoding ascii

# Root-mode provenance: simulate the JSON emitted by tools/virtio-win/extract.py so
# make-driver-pack.ps1 can record ISO hash/path even when -VirtioWinRoot is used.
$fakeIsoPath = "synthetic-virtio-win.iso"
$fakeIsoSha = ("0123456789abcdef" * 4)
$fakeIsoVolumeId = "SYNTH_VIRTIOWIN"
@{
  schema_version = 1
  virtio_win_iso = @{
    path = $fakeIsoPath
    sha256 = $fakeIsoSha
    volume_id = $fakeIsoVolumeId
  }
} | ConvertTo-Json -Depth 4 | Out-File -FilePath (Join-Path $syntheticRoot "virtio-win-provenance.json") -Encoding UTF8

# viostor: include an inline comment immediately after the service token to ensure AddService parsing
# strips INF comments (e.g. `AddService = viostor; comment, ...` should yield service name `viostor`).
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1042" -AddServiceInlineComment
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1042" -AddServiceInlineComment

# netkvm: write UTF-16LE without BOM to ensure make-guest-tools-from-virtio-win.ps1 can derive
# AddService names from BOM-less UTF-16 INFs.
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1041" -InfUtf16LeNoBom
New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1041" -InfUtf16LeNoBom

if (-not $OmitOptionalDrivers) {
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1059"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1059"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1052"
  New-SyntheticDriverFiles -VirtioRoot $syntheticRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1052"
}

$driverPackOutDir = Join-Path $OutRoot "driver-pack-out"
Ensure-EmptyDirectory -Path $driverPackOutDir

$driverPackScript = Join-Path $repoRoot "drivers\scripts\make-driver-pack.ps1"
$driverPackLog = Join-Path $logsDir "make-driver-pack.log"

Write-Host "Running make-driver-pack.ps1..."
& pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
  -VirtioWinRoot $syntheticRoot `
  -OutDir $driverPackOutDir `
  -NoZip *>&1 | Tee-Object -FilePath $driverPackLog
if ($LASTEXITCODE -ne 0) {
  throw "make-driver-pack.ps1 failed (exit $LASTEXITCODE). See $driverPackLog"
}

$driverPackRoot = Join-Path $driverPackOutDir "aero-win7-driver-pack"
if (-not (Test-Path -LiteralPath $driverPackRoot -PathType Container)) {
  throw "Expected driver pack staging directory not found: $driverPackRoot"
}

foreach ($p in @(
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.inf"),
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.sys"),
  (Join-Path $driverPackRoot "win7\x86\viostor\viostor.cat"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.inf"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.sys"),
  (Join-Path $driverPackRoot "win7\x86\netkvm\netkvm.cat"),
  (Join-Path $driverPackRoot "win7\amd64\viostor\viostor.inf"),
  (Join-Path $driverPackRoot "win7\amd64\netkvm\netkvm.inf"),
  (Join-Path $driverPackRoot "install.cmd"),
  (Join-Path $driverPackRoot "enable-testsigning.cmd"),
  (Join-Path $driverPackRoot "THIRD_PARTY_NOTICES.md"),
  (Join-Path $driverPackRoot "README.md"),
  (Join-Path (Join-Path (Join-Path $driverPackRoot "licenses") "virtio-win") "license.txt"),
  (Join-Path (Join-Path (Join-Path $driverPackRoot "licenses") "virtio-win") "notice.txt"),
  (Join-Path $driverPackRoot "manifest.json")
 )) {
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "Expected driver pack output missing: $p"
  }
}

$driverPackManifestPath = Join-Path $driverPackRoot "manifest.json"
$driverPackManifest = Get-Content -LiteralPath $driverPackManifestPath -Raw | ConvertFrom-Json
if ($driverPackManifest.source.path -ne $fakeIsoPath) {
  throw "Driver pack manifest source.path mismatch: expected '$fakeIsoPath', got '$($driverPackManifest.source.path)'"
}
if ($driverPackManifest.source.derived_version -ne $fakeVirtioWinVersion) {
  throw "Driver pack manifest source.derived_version mismatch: expected '$fakeVirtioWinVersion', got '$($driverPackManifest.source.derived_version)'"
}
if (-not $driverPackManifest.source.hash -or $driverPackManifest.source.hash.value -ne $fakeIsoSha) {
  throw "Driver pack manifest source.hash mismatch: expected '$fakeIsoSha', got '$($driverPackManifest.source.hash.value)'"
}
if ($driverPackManifest.source.hash.algorithm -ne "sha256") {
  throw "Driver pack manifest source.hash.algorithm mismatch: expected 'sha256', got '$($driverPackManifest.source.hash.algorithm)'"
}
if ($driverPackManifest.source.volume_label -ne $fakeIsoVolumeId) {
  throw "Driver pack manifest source.volume_label mismatch: expected '$fakeIsoVolumeId', got '$($driverPackManifest.source.volume_label)'"
}
$noticeCopied = @($driverPackManifest.source.license_notice_files_copied)
foreach ($want in @("license.txt", "notice.txt")) {
  if (-not ($noticeCopied -contains $want)) {
    throw "Driver pack manifest did not record copied notice file '$want' in source.license_notice_files_copied. Got: $($noticeCopied -join ', ')"
  }
}

#
# Contract auto-patching smoke test:
# Validate `ci/package-guest-tools.ps1` can auto-align Windows device contract driver_service_name
# values with the staged INF AddService names for all virtio devices (not just virtio-blk).
#
# This specifically exercises the "package Guest Tools from external driver bundles" flow where
# the caller might pass a contract whose service names are stale/incorrect for the staged drivers.
#
$contractPatchDriversRoot = Join-Path $OutRoot "guest-tools-packager-drivers"
Ensure-EmptyDirectory -Path $contractPatchDriversRoot
Ensure-Directory -Path (Join-Path $contractPatchDriversRoot "x86")
Ensure-Directory -Path (Join-Path $contractPatchDriversRoot "amd64")

function Copy-DriverPackArchToPackagerLayout {
  param(
    [Parameter(Mandatory = $true)][string]$SourceArchDir,
    [Parameter(Mandatory = $true)][string]$DestArchDir
  )

  $children = Get-ChildItem -LiteralPath $SourceArchDir -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name
  foreach ($d in $children) {
    $dst = Join-Path $DestArchDir $d.Name
    Copy-Item -LiteralPath $d.FullName -Destination $dst -Recurse -Force
  }
}

$driverPackWin7Root = Join-Path $driverPackRoot "win7"
Copy-DriverPackArchToPackagerLayout -SourceArchDir (Join-Path $driverPackWin7Root "x86") -DestArchDir (Join-Path $contractPatchDriversRoot "x86")
Copy-DriverPackArchToPackagerLayout -SourceArchDir (Join-Path $driverPackWin7Root "amd64") -DestArchDir (Join-Path $contractPatchDriversRoot "amd64")

$contractTemplate = Join-Path $repoRoot "docs\\windows-device-contract-virtio-win.json"
if (-not (Test-Path -LiteralPath $contractTemplate -PathType Leaf)) {
  throw "Expected virtio-win device contract not found: $contractTemplate"
}

$contractMismatchPath = Join-Path $OutRoot "windows-device-contract-mismatch.json"
$contractObj = Get-Content -LiteralPath $contractTemplate -Raw | ConvertFrom-Json

# Always mismatch boot-critical storage + network.
Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-blk" -ServiceName "mismatch_viostor"
Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-net" -ServiceName "mismatch_netkvm"

# Only mismatch optional devices when the corresponding driver is present in the staged tree; if the
# driver is absent we keep the correct values so the smoke test can run with -OmitOptionalDrivers.
$hasVioinput = Test-Path -LiteralPath (Join-Path (Join-Path $contractPatchDriversRoot "x86") "vioinput\\vioinput.inf")
$hasViosnd = Test-Path -LiteralPath (Join-Path (Join-Path $contractPatchDriversRoot "x86") "viosnd\\viosnd.inf")
if ($hasVioinput) { Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-input" -ServiceName "mismatch_vioinput" }
if ($hasViosnd) { Set-ContractDriverServiceName -Contract $contractObj -DeviceName "virtio-snd" -ServiceName "mismatch_viosnd" }

# Sanity-check: ensure we actually wrote a contract that does NOT match the INF AddService names
# we're going to assert from the packaged ZIP.
foreach ($pair in @(
  @{ Device = "virtio-blk"; Expected = "viostor" },
  @{ Device = "virtio-net"; Expected = "netkvm" }
)) {
  foreach ($d in @($contractObj.devices)) {
    if ($d -and (("" + $d.device).ToLowerInvariant() -eq $pair.Device)) {
      $svc = ("" + $d.driver_service_name).Trim()
      if ($svc.ToLowerInvariant() -eq $pair.Expected) {
        throw "Smoke test setup failure: contract mismatch file already has $($pair.Device) driver_service_name='$svc' (expected it to differ from $($pair.Expected))."
      }
    }
  }
}

$contractJson = $contractObj | ConvertTo-Json -Depth 50
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($contractMismatchPath, ($contractJson + "`n"), $utf8NoBom)

$contractPatchOutDir = Join-Path $OutRoot "guest-tools-contract-patch"
Ensure-EmptyDirectory -Path $contractPatchOutDir

$packageGuestTools = Join-Path $repoRoot "ci\\package-guest-tools.ps1"
if (-not (Test-Path -LiteralPath $packageGuestTools -PathType Leaf)) {
  throw "Expected Guest Tools packaging wrapper not found: $packageGuestTools"
}

$contractPatchLog = Join-Path $logsDir "package-guest-tools-contract-patch.log"
$contractPatchSpec = Join-Path $repoRoot "tools\\packaging\\specs\\win7-virtio-full.json"
Write-Host "Running package-guest-tools.ps1 (contract auto-patching)..."
& pwsh -NoProfile -ExecutionPolicy Bypass -File $packageGuestTools `
  -InputRoot $contractPatchDriversRoot `
  -GuestToolsDir (Join-Path $repoRoot "guest-tools") `
  -SigningPolicy "none" `
  -WindowsDeviceContractPath $contractMismatchPath `
  -SpecPath $contractPatchSpec `
  -OutDir $contractPatchOutDir `
  -Version "0.0.0" `
  -BuildId "ci-contract-patch" *>&1 | Tee-Object -FilePath $contractPatchLog
if ($LASTEXITCODE -ne 0) {
  throw "package-guest-tools.ps1 (contract auto-patching) failed (exit $LASTEXITCODE). See $contractPatchLog"
}

$contractPatchZip = Join-Path $contractPatchOutDir "aero-guest-tools.zip"
if (-not (Test-Path -LiteralPath $contractPatchZip -PathType Leaf)) {
  throw "Expected Guest Tools ZIP output missing (contract auto-patching run): $contractPatchZip"
}

Assert-GuestToolsDevicesCmdServices -ZipPath $contractPatchZip -SpecPath $contractPatchSpec

if ($TestIsoMode) {
  $winPs = Resolve-WindowsPowerShell
  if (-not $winPs) {
    throw "powershell.exe not found; required for -TestIsoMode (New-IsoFile.ps1 + Mount-DiskImage)."
  }

  $isoBuilder = [System.IO.Path]::Combine($repoRoot, "ci", "lib", "New-IsoFile.ps1")
  if (-not (Test-Path -LiteralPath $isoBuilder -PathType Leaf)) {
    throw "Expected ISO builder script not found: $isoBuilder"
  }

  $virtioIsoLabel = "VIRTIO_WIN"
  $virtioIsoPath = Join-Path $OutRoot "virtio-win-synthetic.iso"
  $isoBuildLog = Join-Path $logsDir "make-synthetic-virtio-win-iso.log"

  Write-Host "Building synthetic virtio-win ISO..."
  & $winPs -NoProfile -ExecutionPolicy Bypass -File $isoBuilder `
    -SourcePath $syntheticRoot `
    -IsoPath $virtioIsoPath `
    -VolumeLabel $virtioIsoLabel *>&1 | Tee-Object -FilePath $isoBuildLog
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to build synthetic virtio-win ISO (exit $LASTEXITCODE). See $isoBuildLog"
  }
  if (-not (Test-Path -LiteralPath $virtioIsoPath -PathType Leaf)) {
    throw "Expected synthetic virtio-win ISO not found: $virtioIsoPath"
  }

  $virtioIsoPathResolved = (Resolve-Path -LiteralPath $virtioIsoPath).Path
  $virtioIsoSha = (Get-FileHash -Algorithm SHA256 -Path $virtioIsoPathResolved).Hash.ToLowerInvariant()

  $isoDriverPackOutDir = Join-Path $OutRoot "driver-pack-out-from-iso"
  Ensure-EmptyDirectory -Path $isoDriverPackOutDir
  $isoDriverPackLog = Join-Path $logsDir "make-driver-pack-from-iso.log"

  Write-Host "Running make-driver-pack.ps1 from ISO..."
  & $winPs -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
    -VirtioWinIso $virtioIsoPathResolved `
    -OutDir $isoDriverPackOutDir `
    -NoZip *>&1 | Tee-Object -FilePath $isoDriverPackLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-driver-pack.ps1 failed when invoked with -VirtioWinIso (exit $LASTEXITCODE). See $isoDriverPackLog"
  }

  $isoDriverPackRoot = Join-Path $isoDriverPackOutDir "aero-win7-driver-pack"
  if (-not (Test-Path -LiteralPath $isoDriverPackRoot -PathType Container)) {
    throw "Expected ISO-mode driver pack staging directory not found: $isoDriverPackRoot"
  }

  $isoManifestPath = Join-Path $isoDriverPackRoot "manifest.json"
  if (-not (Test-Path -LiteralPath $isoManifestPath -PathType Leaf)) {
    throw "Expected ISO-mode driver pack manifest not found: $isoManifestPath"
  }

  $isoManifest = Get-Content -LiteralPath $isoManifestPath -Raw | ConvertFrom-Json
  if ($isoManifest.source.path -ne $virtioIsoPathResolved) {
    throw "ISO-mode driver pack manifest source.path mismatch: expected '$virtioIsoPathResolved', got '$($isoManifest.source.path)'"
  }
  if (-not $isoManifest.source.hash -or $isoManifest.source.hash.value -ne $virtioIsoSha) {
    throw "ISO-mode driver pack manifest sha256 mismatch: expected '$virtioIsoSha', got '$($isoManifest.source.hash.value)'"
  }
  if ($isoManifest.source.hash.algorithm -ne "sha256") {
    throw "ISO-mode driver pack manifest hash.algorithm mismatch: expected 'sha256', got '$($isoManifest.source.hash.algorithm)'"
  }
  if ($isoManifest.source.volume_label -ne $virtioIsoLabel) {
    throw "ISO-mode driver pack manifest volume_label mismatch: expected '$virtioIsoLabel', got '$($isoManifest.source.volume_label)'"
  }
  if ($isoManifest.source.derived_version -ne $fakeVirtioWinVersion) {
    throw "ISO-mode driver pack manifest derived_version mismatch: expected '$fakeVirtioWinVersion', got '$($isoManifest.source.derived_version)'"
  }

  $isoNoticeCopied = @($isoManifest.source.license_notice_files_copied)
  foreach ($want in @("license.txt", "notice.txt")) {
    if (-not ($isoNoticeCopied -contains $want)) {
      throw "ISO-mode driver pack manifest did not record copied notice file '$want' in source.license_notice_files_copied. Got: $($isoNoticeCopied -join ', ')"
    }
    $noticePath = Join-Path (Join-Path (Join-Path $isoDriverPackRoot "licenses") "virtio-win") $want
    if (-not (Test-Path -LiteralPath $noticePath -PathType Leaf)) {
      throw "Expected ISO-mode driver pack to include notice file: $noticePath"
    }
  }

  if ($isoManifest.optional_drivers_missing_any) {
    $missingNames = @($isoManifest.optional_drivers_missing | ForEach-Object { $_.name })
    throw "Expected ISO-mode driver pack to include optional drivers, but optional_drivers_missing_any=true. Missing: $($missingNames -join ', ')"
  }
  $isoDriversRequested = @($isoManifest.drivers_requested | ForEach-Object { $_.ToString().ToLowerInvariant() })
  foreach ($want in @("viostor", "netkvm", "viosnd", "vioinput")) {
    if (-not ($isoDriversRequested -contains $want)) {
      throw "Expected ISO-mode driver pack to request driver '$want'. Got: $($isoDriversRequested -join ', ')"
    }
  }
}

if ($OmitOptionalDrivers) {
  if (-not $driverPackManifest.optional_drivers_missing_any) {
    throw "Expected make-driver-pack.ps1 to report optional drivers missing, but optional_drivers_missing_any=false."
  }
  $missingNames = @($driverPackManifest.optional_drivers_missing | ForEach-Object { $_.name })
  foreach ($want in @("viosnd", "vioinput")) {
    if (-not ($missingNames -contains $want)) {
      throw "Expected make-driver-pack.ps1 to report missing optional driver '$want'. Reported: $($missingNames -join ', ')"
    }
  }

  # -StrictOptional is intended to catch missing optional drivers when they're explicitly requested.
  Write-Host "Validating -StrictOptional rejects missing optional drivers..."
  $strictPackOutDir = Join-Path $OutRoot "driver-pack-out-strict"
  Ensure-EmptyDirectory -Path $strictPackOutDir
  $strictPackLog = Join-Path $logsDir "make-driver-pack-strict.log"
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
    -VirtioWinRoot $syntheticRoot `
    -OutDir $strictPackOutDir `
    -Drivers "viostor","netkvm","viosnd","vioinput" `
    -StrictOptional `
    -NoZip *>&1 | Tee-Object -FilePath $strictPackLog
  if ($LASTEXITCODE -eq 0) {
    throw "Expected make-driver-pack.ps1 -StrictOptional to fail when optional drivers are missing. See $strictPackLog"
  }

  # When optional drivers are unavailable, callers should still be able to build a pack by
  # explicitly requesting only the required drivers.
  Write-Host "Validating -Drivers viostor,netkvm succeeds without optional drivers..."
  $requiredOnlyPackOutDir = Join-Path $OutRoot "driver-pack-out-required-only"
  Ensure-EmptyDirectory -Path $requiredOnlyPackOutDir
  $requiredOnlyPackLog = Join-Path $logsDir "make-driver-pack-required-only.log"
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
    -VirtioWinRoot $syntheticRoot `
    -OutDir $requiredOnlyPackOutDir `
    -Drivers "viostor","netkvm" `
    -NoZip *>&1 | Tee-Object -FilePath $requiredOnlyPackLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-driver-pack.ps1 failed with -Drivers viostor,netkvm (exit $LASTEXITCODE). See $requiredOnlyPackLog"
  }

  $requiredOnlyPackRoot = Join-Path $requiredOnlyPackOutDir "aero-win7-driver-pack"
  if (-not (Test-Path -LiteralPath $requiredOnlyPackRoot -PathType Container)) {
    throw "Expected driver pack staging directory not found: $requiredOnlyPackRoot"
  }

  $requiredOnlyManifestPath = Join-Path $requiredOnlyPackRoot "manifest.json"
  if (-not (Test-Path -LiteralPath $requiredOnlyManifestPath -PathType Leaf)) {
    throw "Expected driver pack manifest not found: $requiredOnlyManifestPath"
  }
  $requiredOnlyManifest = Get-Content -LiteralPath $requiredOnlyManifestPath -Raw | ConvertFrom-Json
  if ($requiredOnlyManifest.optional_drivers_missing_any) {
    throw "Expected -Drivers viostor,netkvm manifest to have optional_drivers_missing_any=false"
  }
  $driversRequested = @($requiredOnlyManifest.drivers_requested | ForEach-Object { $_.ToString().ToLowerInvariant() })
  foreach ($want in @("viostor", "netkvm")) {
    if (-not ($driversRequested -contains $want)) {
      throw "Expected -Drivers viostor,netkvm to request '$want'. Got: $($driversRequested -join ', ')"
    }
  }
  foreach ($notWant in @("viosnd", "vioinput")) {
    if ($driversRequested -contains $notWant) {
      throw "Did not expect -Drivers viostor,netkvm to request '$notWant'. Got: $($driversRequested -join ', ')"
    }
    if (Test-Path -LiteralPath (Join-Path $requiredOnlyPackRoot "win7\\x86\\$notWant") -PathType Container) {
      throw "Did not expect optional driver directory to be created: win7/x86/$notWant"
    }
  }
} else {
  if ($driverPackManifest.optional_drivers_missing_any) {
    $missingNames = @($driverPackManifest.optional_drivers_missing | ForEach-Object { $_.name })
    throw "Expected make-driver-pack.ps1 to include optional drivers, but optional_drivers_missing_any=true. Missing: $($missingNames -join ', ')"
  }
  $driversIncluded = @($driverPackManifest.drivers | ForEach-Object { $_.ToString().ToLowerInvariant() })
  foreach ($want in @("viosnd", "vioinput")) {
    if (-not ($driversIncluded -contains $want)) {
      throw "Expected make-driver-pack.ps1 to include optional driver '$want' when present. Included drivers: $($driversIncluded -join ', ')"
    }
  }
}

$guestToolsOutDir = Join-Path $OutRoot "guest-tools-out"
Ensure-EmptyDirectory -Path $guestToolsOutDir

$guestToolsScript = Join-Path $repoRoot "drivers\scripts\make-guest-tools-from-virtio-win.ps1"
$guestToolsLog = Join-Path $logsDir "make-guest-tools-from-virtio-win.log"

Write-Host "Running make-guest-tools-from-virtio-win.ps1..."
$guestToolsArgs = @(
  "-OutDir", $guestToolsOutDir,
  "-Profile", $resolvedGuestToolsProfile,
  "-SpecPath", $GuestToolsSpecPath,
  "-Version", "0.0.0",
  "-BuildId", "ci",
  "-CleanStage"
)
if ($TestIsoMode) {
  $guestToolsArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
} else {
  $guestToolsArgs += @("-VirtioWinRoot", $syntheticRoot)
}
& pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsArgs *>&1 | Tee-Object -FilePath $guestToolsLog
if ($LASTEXITCODE -ne 0) {
  throw "make-guest-tools-from-virtio-win.ps1 failed (exit $LASTEXITCODE). See $guestToolsLog"
}

$guestIso = Join-Path $guestToolsOutDir "aero-guest-tools.iso"
$guestZip = Join-Path $guestToolsOutDir "aero-guest-tools.zip"
$guestManifest = Join-Path $guestToolsOutDir "manifest.json"

foreach ($p in @($guestIso, $guestZip, $guestManifest)) {
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "Expected Guest Tools output missing: $p"
  }
}

$manifestObj = Get-Content -LiteralPath $guestManifest -Raw | ConvertFrom-Json
if ($manifestObj.package.version -ne "0.0.0") {
  throw "Guest Tools manifest version mismatch: expected 0.0.0, got $($manifestObj.package.version)"
}
if ($manifestObj.package.build_id -ne "ci") {
  throw "Guest Tools manifest build_id mismatch: expected ci, got $($manifestObj.package.build_id)"
}
if ($manifestObj.signing_policy -ne "none") {
  throw "Guest Tools manifest signing_policy mismatch: expected none, got $($manifestObj.signing_policy)"
}
if ($manifestObj.certs_required -ne $false) {
  throw "Guest Tools manifest certs_required mismatch: expected false, got $($manifestObj.certs_required)"
}

$manifestPaths = @($manifestObj.files | ForEach-Object { $_.path })
foreach ($p in $manifestPaths) {
  if ($p -like "certs/*" -and $p -ne "certs/README.md") {
    throw "Did not expect certificate files to be packaged for signing_policy=none: $p"
  }
}
foreach ($want in @(
  "THIRD_PARTY_NOTICES.md",
  "licenses/virtio-win/license.txt",
  "licenses/virtio-win/notice.txt",
  "licenses/virtio-win/driver-pack-manifest.json",
  "drivers/x86/viostor/viostor.inf",
  "drivers/amd64/viostor/viostor.inf",
  "drivers/x86/netkvm/netkvm.inf",
  "drivers/amd64/netkvm/netkvm.inf"
)) {
  if (-not ($manifestPaths -contains $want)) {
    throw "Guest Tools manifest missing expected packaged file path: $want"
  }
}

# Validate that the packaged `config/devices.cmd` uses virtio-win-correct service names
# (matching the packaged INF AddService directives), so `guest-tools/setup.cmd` can
# perform boot-critical storage validation + pre-seeding without /skipstorage.
Assert-GuestToolsDevicesCmdServices -ZipPath $guestZip -SpecPath $GuestToolsSpecPath

# Regression test: ci/package-guest-tools.ps1 patches the Windows device contract by
# scanning driver INFs for HWIDs + AddService lines. Some upstream driver bundles ship
# UTF-16LE INFs without a BOM; ensure the wrapper's INF parsing handles this encoding.
Write-Host "Running package-guest-tools.ps1 UTF-16LE(no BOM) INF auto-patching smoke test..."

$utf16SmokeRoot = Join-Path $OutRoot "guest-tools-utf16-inf-nobom"
Ensure-EmptyDirectory -Path $utf16SmokeRoot

$utf16DriversRoot = Join-Path $utf16SmokeRoot "drivers"
Ensure-Directory -Path $utf16DriversRoot
Ensure-Directory -Path (Join-Path $utf16DriversRoot "x86")
Ensure-Directory -Path (Join-Path $utf16DriversRoot "amd64")

$utf16HwidUnique = "PCI\VEN_1AF4&DEV_1042&SUBSYS_DEADBEEF"

# viostor (x86): include both an ASCII INF (for packager HWID validation) and a UTF-16LE(no BOM)
# INF containing a more specific HWID. The contract auto-patcher should match the latter.
$viostorUtf16X86Dir = Join-Path (Join-Path $utf16DriversRoot "x86") "viostor"
Ensure-Directory -Path $viostorUtf16X86Dir
Write-SyntheticInf -Path (Join-Path $viostorUtf16X86Dir "viostor.inf") -BaseName "viostor" -HardwareId "PCI\VEN_1AF4&DEV_1042"
Write-SyntheticInfUtf16LeNoBom -Path (Join-Path $viostorUtf16X86Dir "viostor-utf16-nobom.inf") -BaseName "viostor" -HardwareId $utf16HwidUnique
Write-PlaceholderBinary -Path (Join-Path $viostorUtf16X86Dir "viostor.sys")
Write-PlaceholderBinary -Path (Join-Path $viostorUtf16X86Dir "viostor.cat")

# viostor (amd64): keep a normal ASCII INF; it should not match the unique HWID used for patching.
$viostorUtf16Amd64Dir = Join-Path (Join-Path $utf16DriversRoot "amd64") "viostor"
Ensure-Directory -Path $viostorUtf16Amd64Dir
Write-SyntheticInf -Path (Join-Path $viostorUtf16Amd64Dir "viostor.inf") -BaseName "viostor" -HardwareId "PCI\VEN_1AF4&DEV_1042"
Write-PlaceholderBinary -Path (Join-Path $viostorUtf16Amd64Dir "viostor.sys")
Write-PlaceholderBinary -Path (Join-Path $viostorUtf16Amd64Dir "viostor.cat")

# netkvm (required by win7-virtio-win.json).
foreach ($arch in @("x86", "amd64")) {
  $dir = Join-Path (Join-Path $utf16DriversRoot $arch) "netkvm"
  Ensure-Directory -Path $dir
  Write-SyntheticInf -Path (Join-Path $dir "netkvm.inf") -BaseName "netkvm" -HardwareId "PCI\VEN_1AF4&DEV_1041"
  Write-PlaceholderBinary -Path (Join-Path $dir "netkvm.sys")
  Write-PlaceholderBinary -Path (Join-Path $dir "netkvm.cat")
}

# Synthetic contract based on the canonical Aero contract, but force virtio-blk to start with
# the wrong service name and a HWID that only exists in the UTF-16(no BOM) INF above.
$contractTemplatePath = Join-Path $repoRoot "docs\\windows-device-contract.json"
$utf16ContractPath = Join-Path $utf16SmokeRoot "windows-device-contract.json"
$contractObj = Get-Content -LiteralPath $contractTemplatePath -Raw | ConvertFrom-Json
$patched = $false
foreach ($d in @($contractObj.devices)) {
  if ($d -and $d.device -and ($d.device -ieq "virtio-blk")) {
    $d.driver_service_name = "wrongsvc"
    $d.hardware_id_patterns = @($utf16HwidUnique)
    $patched = $true
    break
  }
}
if (-not $patched) {
  throw "Synthetic contract generation failed: missing virtio-blk entry in $contractTemplatePath"
}
$contractJson = $contractObj | ConvertTo-Json -Depth 50
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($utf16ContractPath, ($contractJson + "`n"), $utf8NoBom)

$utf16SpecPath = Join-Path $repoRoot "tools\\packaging\\specs\\win7-virtio-win.json"
if (-not (Test-Path -LiteralPath $utf16SpecPath -PathType Leaf)) {
  throw "Expected packaging spec not found: $utf16SpecPath"
}

$utf16OutDir = Join-Path $utf16SmokeRoot "out"
Ensure-EmptyDirectory -Path $utf16OutDir
$utf16PackLog = Join-Path $logsDir "package-guest-tools-utf16-inf-nobom.log"
$wrapperScript = Join-Path $repoRoot "ci\\package-guest-tools.ps1"

& pwsh -NoProfile -ExecutionPolicy Bypass -File $wrapperScript `
  -InputRoot $utf16DriversRoot `
  -GuestToolsDir (Join-Path $repoRoot "guest-tools") `
  -SigningPolicy "none" `
  -SpecPath $utf16SpecPath `
  -WindowsDeviceContractPath $utf16ContractPath `
  -OutDir $utf16OutDir `
  -Version "0.0.0" `
  -BuildId "ci-utf16-inf-nobom" *>&1 | Tee-Object -FilePath $utf16PackLog
if ($LASTEXITCODE -ne 0) {
  throw "package-guest-tools.ps1 UTF-16(no BOM) INF smoke test failed (exit $LASTEXITCODE). See $utf16PackLog"
}

$utf16Zip = Join-Path $utf16OutDir "aero-guest-tools.zip"
if (-not (Test-Path -LiteralPath $utf16Zip -PathType Leaf)) {
  throw "Expected Guest Tools ZIP not found (UTF-16(no BOM) INF smoke test): $utf16Zip"
}
$utf16DevicesCmd = ReadZipEntryText -ZipPath $utf16Zip -EntryPath "config/devices.cmd"
Assert-DevicesCmdVarEquals -DevicesCmdText $utf16DevicesCmd -VarName "AERO_VIRTIO_BLK_SERVICE" -Expected "viostor"

$specDriverNames = Get-SpecDriverNames -SpecPath $GuestToolsSpecPath

# Regression test: upstream virtio-win optional drivers may be present for only one architecture.
# `make-driver-pack.ps1` must omit any such *partial* optional drivers from *both* arches
# (best-effort), so `aero_packager` never sees a one-arch-only optional driver directory
# (require_optional_drivers_on_all_arches=true).
if ($resolvedGuestToolsProfile -eq "full") {
  Write-Host "Running partial optional-driver normalization smoke test (optional drivers x86-only)..."

  $syntheticPartialRoot = Join-Path $OutRoot "virtio-win-partial-optional"
  Ensure-EmptyDirectory -Path $syntheticPartialRoot

  # Root-level license/notice files (best-effort copy). Use lowercase filenames to ensure
  # packaging is robust on case-sensitive filesystems.
  "license placeholder" | Out-File -FilePath (Join-Path $syntheticPartialRoot "license.txt") -Encoding ascii
  "notice placeholder" | Out-File -FilePath (Join-Path $syntheticPartialRoot "notice.txt") -Encoding ascii
  $fakeVirtioWinVersionPartial = "0.0.0-synthetic-partial-optional"
  $fakeVirtioWinVersionPartial | Out-File -FilePath (Join-Path $syntheticPartialRoot "VERSION") -Encoding ascii

  # Root-mode provenance: simulate the JSON emitted by tools/virtio-win/extract.py so
  # make-driver-pack.ps1 can record ISO hash/path even when -VirtioWinRoot is used.
  $fakeIsoPathPartial = "synthetic-virtio-win-partial.iso"
  $fakeIsoShaPartial = ("abcdef0123456789" * 4)
  $fakeIsoVolumeIdPartial = "SYNTH_VIRTIO_PARTIAL"
  @{
    schema_version = 1
    virtio_win_iso = @{
      path = $fakeIsoPathPartial
      sha256 = $fakeIsoShaPartial
      volume_id = $fakeIsoVolumeIdPartial
    }
  } | ConvertTo-Json -Depth 4 | Out-File -FilePath (Join-Path $syntheticPartialRoot "virtio-win-provenance.json") -Encoding UTF8

  # Required drivers.
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1042"
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "viostor" -InfBaseName "viostor" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1042"
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1041"
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "NetKVM" -InfBaseName "netkvm" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1041"

  # Optional drivers: partial (one-arch-only) -> should be omitted entirely by make-driver-pack.ps1.
  #
  # Cover both directions:
  # - viosnd present only for x86
  # - vioinput present only for amd64
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "viosnd" -InfBaseName "viosnd" -OsDirName $osDir -ArchDirName "x86" -HardwareId "PCI\VEN_1AF4&DEV_1059"
  New-SyntheticDriverFiles -VirtioRoot $syntheticPartialRoot -UpstreamDirName "vioinput" -InfBaseName "vioinput" -OsDirName $osDir -ArchDirName "amd64" -HardwareId "PCI\VEN_1AF4&DEV_1052"

  $guestToolsPartialOutDir = Join-Path $OutRoot "guest-tools-partial-optional"
  Ensure-EmptyDirectory -Path $guestToolsPartialOutDir
  $guestToolsPartialLog = Join-Path $logsDir "make-guest-tools-from-virtio-win-partial-optional.log"

  # Pass a *relative* spec path to explicitly exercise make-guest-tools-from-virtio-win.ps1's
  # repo-root resolution behaviour for this regression scenario.
  $guestToolsFullSpecRel = "tools/packaging/specs/win7-virtio-full.json"
  $guestToolsFullSpecAbs = Join-Path $repoRoot $guestToolsFullSpecRel
  if (-not (Test-Path -LiteralPath $guestToolsFullSpecAbs -PathType Leaf)) {
    throw "Expected packaging spec not found for partial optional-driver smoke test: $guestToolsFullSpecAbs"
  }

  $guestToolsPartialArgs = @(
    "-OutDir", $guestToolsPartialOutDir,
    "-Profile", "full",
    "-SpecPath", $guestToolsFullSpecRel,
    "-Version", "0.0.0",
    "-BuildId", "ci-partial-optional",
    "-CleanStage",
    "-VirtioWinRoot", $syntheticPartialRoot
  )

  & pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsPartialArgs *>&1 | Tee-Object -FilePath $guestToolsPartialLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-guest-tools-from-virtio-win.ps1 failed for partial optional-driver smoke test (exit $LASTEXITCODE). See $guestToolsPartialLog"
  }

  $guestToolsPartialZip = Join-Path $guestToolsPartialOutDir "aero-guest-tools.zip"
  if (-not (Test-Path -LiteralPath $guestToolsPartialZip -PathType Leaf)) {
    throw "Expected Guest Tools ZIP missing for partial optional-driver smoke test: $guestToolsPartialZip"
  }

  foreach ($p in @(
    "drivers/x86/viosnd/viosnd.inf",
    "drivers/amd64/viosnd/viosnd.inf",
    "drivers/x86/vioinput/vioinput.inf",
    "drivers/amd64/vioinput/vioinput.inf"
  )) {
    Assert-ZipNotContainsEntry -ZipPath $guestToolsPartialZip -EntryPath $p
  }

  # Validate the embedded driver-pack manifest reflects the post-normalization result:
  # - optional drivers recorded missing for BOTH arches
  # - not listed as included
  $partialDriverPackManifestText = ReadZipEntryText -ZipPath $guestToolsPartialZip -EntryPath "licenses/virtio-win/driver-pack-manifest.json"
  $partialDriverPackManifest = $partialDriverPackManifestText | ConvertFrom-Json
  if (-not $partialDriverPackManifest.optional_drivers_missing_any) {
    throw "Expected driver-pack manifest to report missing optional drivers for the partial-optional scenario (optional_drivers_missing_any=false)."
  }
  $partialMissing = @($partialDriverPackManifest.optional_drivers_missing)
  foreach ($want in @("viosnd", "vioinput")) {
    $entry = $partialMissing | Where-Object { $_.name -and (($_.name).ToLowerInvariant() -eq $want) } | Select-Object -First 1
    if (-not $entry) {
      throw "Expected driver-pack manifest to report missing optional driver '$want' for the partial-optional scenario."
    }
    $targets = @($entry.missing_targets | ForEach-Object { $_.ToString().ToLowerInvariant() })
    foreach ($t in @("win7-x86", "win7-amd64")) {
      if (-not ($targets -contains $t)) {
        throw "Expected driver-pack manifest optional_drivers_missing entry for '$want' to include missing target '$t'. Got: $($targets -join ', ')"
      }
    }
  }
  $partialIncludedDrivers = @($partialDriverPackManifest.drivers | ForEach-Object { $_.ToString().ToLowerInvariant() })
  foreach ($notWant in @("viosnd", "vioinput")) {
    if ($partialIncludedDrivers -contains $notWant) {
      throw "Did not expect driver-pack manifest to list '$notWant' as included for the partial-optional scenario. Included: $($partialIncludedDrivers -join ', ')"
    }
  }

  # -StrictOptional should reject partial optional drivers (regardless of the best-effort
  # normalization behaviour in the default mode).
  Write-Host "Validating -StrictOptional rejects partial optional drivers..."
  $partialStrictOutDir = Join-Path $OutRoot "driver-pack-partial-optional-strict"
  Ensure-EmptyDirectory -Path $partialStrictOutDir
  $partialStrictLog = Join-Path $logsDir "make-driver-pack-partial-optional-strict.log"
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $driverPackScript `
    -VirtioWinRoot $syntheticPartialRoot `
    -OutDir $partialStrictOutDir `
    -Drivers "viostor","netkvm","viosnd","vioinput" `
    -StrictOptional `
    -NoZip *>&1 | Tee-Object -FilePath $partialStrictLog
  if ($LASTEXITCODE -eq 0) {
    throw "Expected make-driver-pack.ps1 -StrictOptional to fail for partial optional drivers. See $partialStrictLog"
  }
}

# When optional drivers are both present in the synthetic virtio-win tree and declared in the
# Guest Tools packaging spec, they should be included in the packaged output.
if (-not $OmitOptionalDrivers) {
  $optionalChecks = @()
  if ($specDriverNames -contains "viosnd") { $optionalChecks += "drivers/x86/viosnd/viosnd.inf"; $optionalChecks += "drivers/amd64/viosnd/viosnd.inf" }
  if ($specDriverNames -contains "vioinput") { $optionalChecks += "drivers/x86/vioinput/vioinput.inf"; $optionalChecks += "drivers/amd64/vioinput/vioinput.inf" }

  foreach ($want in $optionalChecks) {
    if (-not ($manifestPaths -contains $want)) {
      throw "Guest Tools manifest missing expected optional driver file path: $want"
    }
  }
}

# Exercise the CI Guest Tools packager wrapper with -ExtraToolsDir, ensuring optional
# tools are staged under `tools/` and that debug symbol artifacts are excluded from the
# final packaged outputs.
$ciGuestToolsInputRoot = Join-Path $OutRoot "ci-guest-tools-input"
Ensure-EmptyDirectory -Path $ciGuestToolsInputRoot
foreach ($arch in @("x86", "amd64")) {
  Ensure-Directory -Path (Join-Path $ciGuestToolsInputRoot $arch)
}

$ciDriverSources = @(
  [pscustomobject]@{ Name = "viostor"; Upstream = "viostor" },
  [pscustomobject]@{ Name = "netkvm"; Upstream = "NetKVM" },
  [pscustomobject]@{ Name = "viosnd"; Upstream = "viosnd" },
  [pscustomobject]@{ Name = "vioinput"; Upstream = "vioinput" }
)
foreach ($drv in $ciDriverSources) {
  foreach ($arch in @("x86", "amd64")) {
    $src = Join-Path $syntheticRoot (Join-Path $drv.Upstream (Join-Path $osDir $arch))
    if (-not (Test-Path -LiteralPath $src -PathType Container)) { continue }
    $dest = Join-Path (Join-Path $ciGuestToolsInputRoot $arch) $drv.Name
    Ensure-Directory -Path $dest
    Copy-Item -Path (Join-Path $src "*") -Destination $dest -Recurse -Force
  }
}

foreach ($req in @("viostor", "netkvm")) {
  foreach ($arch in @("x86", "amd64")) {
    $p = Join-Path (Join-Path $ciGuestToolsInputRoot $arch) $req
    if (-not (Test-Path -LiteralPath $p -PathType Container)) {
      throw "CI Guest Tools packaging input missing expected driver directory: $p"
    }
  }
}

$extraToolsDir = Join-Path $OutRoot "extra-tools"
Ensure-EmptyDirectory -Path $extraToolsDir
Write-PlaceholderBinary -Path (Join-Path $extraToolsDir "aerogpu_dbgctl.exe")
Write-PlaceholderBinary -Path (Join-Path $extraToolsDir "aerogpu_dbgctl.pdb")

$ciGuestToolsOutDir = Join-Path $OutRoot "ci-guest-tools-out"
Ensure-EmptyDirectory -Path $ciGuestToolsOutDir
$ciGuestToolsLog = Join-Path $logsDir "package-guest-tools-extra-tools.log"
$ciGuestToolsScript = Join-Path $repoRoot "ci\\package-guest-tools.ps1"

Write-Host "Running package-guest-tools.ps1 (-ExtraToolsDir)..."
$ciGuestToolsArgs = @(
  "-InputRoot", $ciGuestToolsInputRoot,
  "-GuestToolsDir", (Join-Path $repoRoot "guest-tools"),
  "-SigningPolicy", "none",
  "-SpecPath", $GuestToolsSpecPath,
  "-WindowsDeviceContractPath", "docs/windows-device-contract-virtio-win.json",
  "-OutDir", $ciGuestToolsOutDir,
  "-Version", "0.0.0",
  "-BuildId", "ci-extra-tools",
  "-ExtraToolsDir", $extraToolsDir
)
& pwsh -NoProfile -ExecutionPolicy Bypass -File $ciGuestToolsScript @ciGuestToolsArgs *>&1 | Tee-Object -FilePath $ciGuestToolsLog
if ($LASTEXITCODE -ne 0) {
  throw "package-guest-tools.ps1 failed (exit $LASTEXITCODE). See $ciGuestToolsLog"
}

$ciGuestToolsZip = Join-Path $ciGuestToolsOutDir "aero-guest-tools.zip"
if (-not (Test-Path -LiteralPath $ciGuestToolsZip -PathType Leaf)) {
  throw "Expected package-guest-tools.ps1 output ZIP missing: $ciGuestToolsZip"
}

Write-Host "Verifying optional tools are packaged..."
Assert-ZipContainsEntry -ZipPath $ciGuestToolsZip -EntryPath "tools/aerogpu_dbgctl.exe"
Assert-ZipNotContainsEntry -ZipPath $ciGuestToolsZip -EntryPath "tools/aerogpu_dbgctl.pdb"

if ($TestSigningPolicies) {
  $guestToolsTestSigningOutDir = Join-Path $OutRoot "guest-tools-testsigning"
  Ensure-EmptyDirectory -Path $guestToolsTestSigningOutDir
  $guestToolsTestSigningLog = Join-Path $logsDir "make-guest-tools-from-virtio-win-testsigning.log"

  Write-Host "Running make-guest-tools-from-virtio-win.ps1 (signing-policy test)..."
  $guestToolsTestSigningArgs = @(
    "-OutDir", $guestToolsTestSigningOutDir,
    "-Profile", $resolvedGuestToolsProfile,
    "-SpecPath", $GuestToolsSpecPath,
    "-SigningPolicy", "test",
    "-Version", "0.0.0",
    "-BuildId", "ci-testsigning",
    "-CleanStage"
  )
  if ($TestIsoMode) {
    $guestToolsTestSigningArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
  } else {
    $guestToolsTestSigningArgs += @("-VirtioWinRoot", $syntheticRoot)
  }
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsTestSigningArgs *>&1 | Tee-Object -FilePath $guestToolsTestSigningLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-guest-tools-from-virtio-win.ps1 (signing_policy=test) failed (exit $LASTEXITCODE). See $guestToolsTestSigningLog"
  }

  $testSigningManifestPath = Join-Path $guestToolsTestSigningOutDir "manifest.json"
  if (-not (Test-Path -LiteralPath $testSigningManifestPath -PathType Leaf)) {
    throw "Expected Guest Tools test-signing manifest not found: $testSigningManifestPath"
  }
  $testSigningManifest = Get-Content -LiteralPath $testSigningManifestPath -Raw | ConvertFrom-Json
  if ($testSigningManifest.package.build_id -ne "ci-testsigning") {
    throw "Guest Tools test-signing manifest build_id mismatch: expected ci-testsigning, got $($testSigningManifest.package.build_id)"
  }
  $testSigningPolicy = ("" + $testSigningManifest.signing_policy).ToLowerInvariant()
  if (($testSigningPolicy -eq "testsigning") -or ($testSigningPolicy -eq "test-signing")) { $testSigningPolicy = "test" }
  if ($testSigningPolicy -ne "test") {
    throw "Guest Tools test-signing manifest signing_policy mismatch: expected test, got $($testSigningManifest.signing_policy)"
  }
  if ($testSigningManifest.certs_required -ne $true) {
    throw "Guest Tools test-signing manifest certs_required mismatch: expected true, got $($testSigningManifest.certs_required)"
  }
  $testSigningPaths = @($testSigningManifest.files | ForEach-Object { $_.path })
  $testSigningCerts = @($testSigningPaths | Where-Object { $_ -match '^certs/.*\.(cer|crt|p7b)$' })
  if ($testSigningCerts.Count -eq 0) {
    throw "Guest Tools test-signing output contained no certificate artifacts under certs/*.cer|*.crt|*.p7b"
  }

  $testSigningZip = Join-Path $guestToolsTestSigningOutDir "aero-guest-tools.zip"
  if (-not (Test-Path -LiteralPath $testSigningZip -PathType Leaf)) {
    throw "Expected Guest Tools testsigning ZIP not found: $testSigningZip"
  }
  Assert-GuestToolsDevicesCmdServices -ZipPath $testSigningZip -SpecPath $GuestToolsSpecPath
}

if (-not $SkipGuestToolsDefaultsCheck) {
  # Validate the wrapper defaults without explicitly passing -Profile/-SpecPath, so any
  # drift between docs/script defaults is caught by CI.
  $guestToolsDefaultsOutDir = Join-Path $OutRoot "guest-tools-defaults"
  Ensure-EmptyDirectory -Path $guestToolsDefaultsOutDir
  $guestToolsDefaultsLog = Join-Path $logsDir "make-guest-tools-from-virtio-win-defaults.log"

  Write-Host "Running make-guest-tools-from-virtio-win.ps1 (defaults)..."
  $guestToolsDefaultsArgs = @(
    "-OutDir", $guestToolsDefaultsOutDir,
    "-Version", "0.0.0",
    "-BuildId", "ci-defaults",
    "-CleanStage"
  )
  if ($TestIsoMode) {
    $guestToolsDefaultsArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
  } else {
    $guestToolsDefaultsArgs += @("-VirtioWinRoot", $syntheticRoot)
  }
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsDefaultsArgs *>&1 | Tee-Object -FilePath $guestToolsDefaultsLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-guest-tools-from-virtio-win.ps1 (defaults) failed (exit $LASTEXITCODE). See $guestToolsDefaultsLog"
  }

  $defaultsIso = Join-Path $guestToolsDefaultsOutDir "aero-guest-tools.iso"
  $defaultsZip = Join-Path $guestToolsDefaultsOutDir "aero-guest-tools.zip"
  $defaultsManifest = Join-Path $guestToolsDefaultsOutDir "manifest.json"
  foreach ($p in @($defaultsIso, $defaultsZip, $defaultsManifest)) {
    if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
      throw "Expected Guest Tools output missing (defaults run): $p"
    }
  }

  $defaultsLogText = Get-Content -LiteralPath $guestToolsDefaultsLog -Raw
  if ($defaultsLogText -notmatch '(?m)^\s*profile\s*:\s*full\s*$') {
    throw "Expected defaults run to use -Profile full. See $guestToolsDefaultsLog"
  }
  if ($defaultsLogText -notmatch 'win7-virtio-full\.json') {
    throw "Expected defaults run to select win7-virtio-full.json. See $guestToolsDefaultsLog"
  }
  if ($defaultsLogText -notmatch '(?m)^\s*drivers\s*:\s*viostor,\s*netkvm,\s*viosnd,\s*vioinput\s*$') {
    throw "Expected defaults run to extract viostor,netkvm,viosnd,vioinput. See $guestToolsDefaultsLog"
  }

  $defaultsManifestObj = Get-Content -LiteralPath $defaultsManifest -Raw | ConvertFrom-Json
  if ($defaultsManifestObj.package.build_id -ne "ci-defaults") {
    throw "Guest Tools defaults manifest build_id mismatch: expected ci-defaults, got $($defaultsManifestObj.package.build_id)"
  }
  if ($defaultsManifestObj.signing_policy -ne "none") {
    throw "Guest Tools defaults manifest signing_policy mismatch: expected none, got $($defaultsManifestObj.signing_policy)"
  }
  if ($defaultsManifestObj.certs_required -ne $false) {
    throw "Guest Tools defaults manifest certs_required mismatch: expected false, got $($defaultsManifestObj.certs_required)"
  }

  $defaultsPaths = @($defaultsManifestObj.files | ForEach-Object { $_.path })
  foreach ($p in $defaultsPaths) {
    if ($p -like "certs/*" -and $p -ne "certs/README.md") {
      throw "Did not expect certificate files to be packaged for signing_policy=none (defaults run): $p"
    }
  }

  $defaultsSpecPath = Join-Path $repoRoot "tools/packaging/specs/win7-virtio-full.json"
  Assert-GuestToolsDevicesCmdServices -ZipPath $defaultsZip -SpecPath $defaultsSpecPath

  $optionalDriverPaths = @(
    "drivers/x86/viosnd/viosnd.inf",
    "drivers/amd64/viosnd/viosnd.inf",
    "drivers/x86/vioinput/vioinput.inf",
    "drivers/amd64/vioinput/vioinput.inf"
  )

  # Default profile is 'full'. When optional drivers exist in the virtio-win root, they SHOULD be
  # packaged. If this run omits optional drivers, the wrapper should continue best-effort and
  # the packaged output should not contain those files.
  foreach ($p in $optionalDriverPaths) {
    if ($OmitOptionalDrivers) {
      if ($defaultsPaths -contains $p) {
        throw "Did not expect optional driver file path to be packaged when optional drivers are omitted: $p"
      }
    } else {
      if ($defaultsPaths -notcontains $p) {
        throw "Expected optional driver file path to be packaged by default (-Profile full): $p"
      }
    }
  }

  # Validate -Profile minimal selects the minimal spec and extracts only required drivers.
  $guestToolsProfileMinimalOutDir = Join-Path $OutRoot "guest-tools-profile-minimal"
  Ensure-EmptyDirectory -Path $guestToolsProfileMinimalOutDir
  $guestToolsProfileMinimalLog = Join-Path $logsDir "make-guest-tools-from-virtio-win-profile-minimal.log"
  Write-Host "Running make-guest-tools-from-virtio-win.ps1 (-Profile minimal)..."
  $guestToolsProfileMinimalArgs = @(
    "-OutDir", $guestToolsProfileMinimalOutDir,
    "-Profile", "minimal",
    "-Version", "0.0.0",
    "-BuildId", "ci-profile-minimal",
    "-CleanStage"
  )
  if ($TestIsoMode) {
    $guestToolsProfileMinimalArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
  } else {
    $guestToolsProfileMinimalArgs += @("-VirtioWinRoot", $syntheticRoot)
  }
  & pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsProfileMinimalArgs *>&1 | Tee-Object -FilePath $guestToolsProfileMinimalLog
  if ($LASTEXITCODE -ne 0) {
    throw "make-guest-tools-from-virtio-win.ps1 (-Profile minimal) failed (exit $LASTEXITCODE). See $guestToolsProfileMinimalLog"
  }

  $profileMinimalLogText = Get-Content -LiteralPath $guestToolsProfileMinimalLog -Raw
  if ($profileMinimalLogText -notmatch '(?m)^\s*profile\s*:\s*minimal\s*$') {
    throw "Expected -Profile minimal run to use profile=minimal. See $guestToolsProfileMinimalLog"
  }
  if ($profileMinimalLogText -notmatch 'win7-virtio-win\.json') {
    throw "Expected -Profile minimal run to select win7-virtio-win.json. See $guestToolsProfileMinimalLog"
  }
  if ($profileMinimalLogText -notmatch '(?m)^\s*drivers\s*:\s*viostor,\s*netkvm\s*$') {
    throw "Expected -Profile minimal run to extract only viostor,netkvm. See $guestToolsProfileMinimalLog"
  }

  $profileMinimalManifestPath = Join-Path $guestToolsProfileMinimalOutDir "manifest.json"
  if (-not (Test-Path -LiteralPath $profileMinimalManifestPath -PathType Leaf)) {
    throw "Expected Guest Tools -Profile minimal manifest not found: $profileMinimalManifestPath"
  }
  $profileMinimalManifest = Get-Content -LiteralPath $profileMinimalManifestPath -Raw | ConvertFrom-Json
  if ($profileMinimalManifest.package.build_id -ne "ci-profile-minimal") {
    throw "Guest Tools -Profile minimal manifest build_id mismatch: expected ci-profile-minimal, got $($profileMinimalManifest.package.build_id)"
  }
  if ($profileMinimalManifest.signing_policy -ne "none") {
    throw "Guest Tools -Profile minimal manifest signing_policy mismatch: expected none, got $($profileMinimalManifest.signing_policy)"
  }
  $profileMinimalPaths = @($profileMinimalManifest.files | ForEach-Object { $_.path })
  foreach ($p in $profileMinimalPaths) {
    if ($p -like "certs/*" -and $p -ne "certs/README.md") {
      throw "Did not expect certificate files to be packaged for signing_policy=none (-Profile minimal run): $p"
    }
  }
  foreach ($want in $optionalDriverPaths) {
    if ($profileMinimalPaths -contains $want) {
      throw "Did not expect optional driver file path to be packaged for -Profile minimal: $want"
    }
  }

  # Validate that a relative -SpecPath is resolved against the repo root (not the current working directory).
  $guestToolsRelativeSpecOutDir = Join-Path $OutRoot "guest-tools-relative-spec"
  Ensure-EmptyDirectory -Path $guestToolsRelativeSpecOutDir
  $guestToolsRelativeSpecLog = Join-Path $logsDir "make-guest-tools-from-virtio-win-relative-spec.log"
  Write-Host "Running make-guest-tools-from-virtio-win.ps1 (relative -SpecPath)..."
  $guestToolsRelativeSpecArgs = @(
    "-OutDir", $guestToolsRelativeSpecOutDir,
    "-Profile", "full",
    "-SpecPath", "tools/packaging/specs/win7-virtio-full.json",
    "-Version", "0.0.0",
    "-BuildId", "ci-relative-spec",
    "-CleanStage"
  )
  if ($TestIsoMode) {
    $guestToolsRelativeSpecArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
  } else {
    $guestToolsRelativeSpecArgs += @("-VirtioWinRoot", $syntheticRoot)
  }
  Push-Location -LiteralPath $OutRoot
  try {
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $guestToolsScript @guestToolsRelativeSpecArgs *>&1 | Tee-Object -FilePath $guestToolsRelativeSpecLog
  } finally {
    Pop-Location
  }
  if ($LASTEXITCODE -ne 0) {
    throw "make-guest-tools-from-virtio-win.ps1 (relative -SpecPath) failed (exit $LASTEXITCODE). See $guestToolsRelativeSpecLog"
  }

   $relativeSpecLogText = Get-Content -LiteralPath $guestToolsRelativeSpecLog -Raw
   if ($relativeSpecLogText -notmatch '(?m)^\s*profile\s*:\s*full\s*$') {
     throw "Expected relative -SpecPath run to use -Profile full. See $guestToolsRelativeSpecLog"
   }
   if ($relativeSpecLogText -notmatch 'win7-virtio-full\.json') {
     throw "Expected relative -SpecPath run to select win7-virtio-full.json. See $guestToolsRelativeSpecLog"
   }

   $relativeSpecZip = Join-Path $guestToolsRelativeSpecOutDir "aero-guest-tools.zip"
   if (-not (Test-Path -LiteralPath $relativeSpecZip -PathType Leaf)) {
     throw "Expected Guest Tools relative -SpecPath ZIP not found: $relativeSpecZip"
   }
   $relativeSpecPath = Join-Path $repoRoot "tools/packaging/specs/win7-virtio-full.json"
   Assert-GuestToolsDevicesCmdServices -ZipPath $relativeSpecZip -SpecPath $relativeSpecPath
 }

 $isoScript = Join-Path $repoRoot "drivers\scripts\make-virtio-driver-iso.ps1"
 if (-not (Test-Path -LiteralPath $isoScript -PathType Leaf)) {
   throw "Expected script not found: $isoScript"
 }

$python = Resolve-Python
if (-not $python) {
  throw "Python not found on PATH; required for virtio driver ISO validation."
}

$driverIsoPath = Join-Path $OutRoot "aero-virtio-win7-drivers.iso"
$driverIsoLog = Join-Path $logsDir "make-virtio-driver-iso.log"
$verifyIsoLog = Join-Path $logsDir "verify-virtio-driver-iso.log"

Write-Host "Running make-virtio-driver-iso.ps1..."
$driverIsoArgs = @(
  "-OutIso", $driverIsoPath,
  "-CleanStage"
)
if ($TestIsoMode) {
  $driverIsoArgs += @("-VirtioWinIso", $virtioIsoPathResolved)
} else {
  $driverIsoArgs += @("-VirtioWinRoot", $syntheticRoot)
}
& pwsh -NoProfile -ExecutionPolicy Bypass -File $isoScript @driverIsoArgs *>&1 | Tee-Object -FilePath $driverIsoLog
if ($LASTEXITCODE -ne 0) {
  throw "make-virtio-driver-iso.ps1 failed (exit $LASTEXITCODE). See $driverIsoLog"
}

if (-not (Test-Path -LiteralPath $driverIsoPath -PathType Leaf)) {
  throw "Expected driver ISO output missing: $driverIsoPath"
}

Write-Host "Verifying driver ISO contents..."
& $python (Join-Path $repoRoot "tools\driver-iso\verify_iso.py") `
  --iso $driverIsoPath *>&1 | Tee-Object -FilePath $verifyIsoLog
if ($LASTEXITCODE -ne 0) {
  throw "verify_iso.py failed (exit $LASTEXITCODE). See $verifyIsoLog"
}

Write-Host "virtio-win packaging smoke test succeeded."
