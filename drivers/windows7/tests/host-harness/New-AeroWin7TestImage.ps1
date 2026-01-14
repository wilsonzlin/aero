# SPDX-License-Identifier: MIT OR Apache-2.0

[CmdletBinding()]
param(
  # Path to the built guest tool (aero-virtio-selftest.exe)
  [Parameter(Mandatory = $true)]
  [string]$SelftestExePath,

  # Directory containing the Aero virtio driver .inf files (recursively copied onto the provisioning media).
  # This is intentionally flexible: it can point at a build output directory or an extracted driver package.
  [Parameter(Mandatory = $true)]
  [string]$DriversDir,

  # Optional: restrict which .inf files are installed by the guest provisioning script.
  #
  # Each entry can be either:
  # - An INF basename (e.g. "aero_virtio_blk.inf") which must match exactly one file under -DriversDir, or
  # - A relative path under -DriversDir (e.g. "amd64\\viostor\\viostor.inf") to disambiguate duplicates.
  #
  # If not specified, a conservative default allowlist is used to avoid accidentally installing test/smoke INFs
  # that can steal device binding (e.g. virtio-transport-test).
  [Parameter(Mandatory = $false)]
  [string[]]$InfAllowList = @(),

  # Escape hatch: restore legacy behavior (install every .inf found under -DriversDir).
  [Parameter(Mandatory = $false)]
  [switch]$InstallAllInfs,

  # Output directory for provisioning media contents (always generated).
  [Parameter(Mandatory = $false)]
  [string]$OutputDir = "./out/aero-win7-provisioning",

  # If provided, attempt to build an ISO at this path (requires oscdimg or mkisofs/genisoimage).
  [Parameter(Mandatory = $false)]
  [string]$OutputIsoPath = "",

  # Default args baked into the provisioning script (expected by the host harness).
  #
  # Note: the guest virtio-net selftest uses this URL for a basic HTTP connectivity check, and
  # also fetches a deterministic large payload from "<HttpUrl>-large" (1 MiB, bytes 0..255 repeating)
  # to stress sustained TX/RX and validate data integrity.
  [Parameter(Mandatory = $false)]
  [string]$HttpUrl = "http://10.0.2.2:18080/aero-virtio-selftest",

  [Parameter(Mandatory = $false)]
  [string]$DnsHost = "host.lan",

  # Optional: UDP echo server port for the virtio-net UDP smoke test (guest reaches host loopback as 10.0.2.2 via slirp).
  #
  # This is only passed to the guest scheduled task when explicitly specified, so older guest selftest binaries
  # (which do not support the --udp-port argument) can still be provisioned.
  #
  # If you override this, you must also run the host harness with the matching UDP port
  # (PowerShell: Invoke-AeroVirtioWin7Tests.ps1 -UdpPort <port>; Python: invoke_aero_virtio_win7_tests.py --udp-port <port>).
  [Parameter(Mandatory = $false)]
  [ValidateRange(1, 65535)]
  [int]$UdpPort = 18081,

  # Optional: bake a fixed virtio-blk test directory into the scheduled task.
  # Example: "D:\\aero-virtio-selftest\\"
  [Parameter(Mandatory = $false)]
  [string]$BlkRoot = "",

  # If set, require virtio-blk to be operating with MSI/MSI-X (not INTx).
  # This adds `--expect-blk-msi` to the scheduled task.
  #
  # The guest selftest will then fail the virtio-blk test if the driver reports it is still
  # using INTx (e.g. due to MSI/MSI-X not being enabled/working).
  [Parameter(Mandatory = $false)]
  [switch]$ExpectBlkMsi,
  
  # If set, require virtio-net to be operating with MSI-X (not INTx).
  # This adds `--require-net-msix` to the scheduled task.
  #
  # The guest selftest will then fail the overall selftest if virtio-net is not using MSI-X
  # (mode != msix) in its `virtio-net-msix` diagnostic marker.
  #
  # This is the guest-side complement to the host harness requirement flag:
  # - PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -RequireVirtioNetMsix`
  # - Python: `invoke_aero_virtio_win7_tests.py --require-virtio-net-msix`
  [Parameter(Mandatory = $false)]
  [Alias("RequireVirtioNetMsix")]
  [switch]$RequireNetMsix,
  
  # If set, require virtio-input to be operating with MSI-X (not INTx).
  # This adds `--require-input-msix` to the scheduled task.
  #
  # The guest selftest will then fail the overall selftest if virtio-input is not using MSI-X
  # (mode != msix) in its `virtio-input-msix` diagnostic marker.
  #
  # This is the guest-side complement to the host harness requirement flag:
  # - PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -RequireVirtioInputMsix`
  # - Python: `invoke_aero_virtio_win7_tests.py --require-virtio-input-msix`
  [Parameter(Mandatory = $false)]
  [Alias("RequireVirtioInputMsix")]
  [switch]$RequireInputMsix,

  # If set, require virtio-snd to be operating with MSI-X (not INTx).
  # This adds `--require-snd-msix` to the scheduled task.
  #
  # The guest selftest will then fail the overall selftest if virtio-snd is missing, if the optional
  # virtio-snd diagnostics interface is unavailable, or if the driver reports it is not using MSI-X
  # (mode != msix) in its `virtio-snd-msix` diagnostic marker.
  #
  # This is the guest-side complement to the host harness requirement flag:
  # - PowerShell: `Invoke-AeroVirtioWin7Tests.ps1 -RequireVirtioSndMsix`
  # - Python: `invoke_aero_virtio_win7_tests.py --require-virtio-snd-msix`
  [Parameter(Mandatory = $false)]
  [Alias("RequireVirtioSndMsix")]
  [switch]$RequireSndMsix,

  # If set, enable the guest selftest's end-to-end virtio-blk runtime resize test
  # (adds `--test-blk-resize` to the scheduled task).
  #
  # This is required when running the host harness with:
  # - Python: `--with-blk-resize`
  # - PowerShell: `-WithBlkResize`
  [Parameter(Mandatory = $false)]
  [Alias("TestVirtioBlkResize")]
  [switch]$TestBlkResize,
  # For unsigned/test-signed drivers on Windows 7 x64, test-signing mode must be enabled.
  # If set, the provisioning script will run: bcdedit /set testsigning on
  [Parameter(Mandatory = $false)]
  [switch]$EnableTestSigning,

  # If set (and typically used with -EnableTestSigning), the provisioning script will reboot the VM at the end.
  [Parameter(Mandatory = $false)]
  [switch]$AutoReboot,

  # If set, require virtio-snd in the guest selftest (adds `--require-snd` to the scheduled task).
  # Kept for backwards compatibility with older automation that passed -RequireSnd.
  [Parameter(Mandatory = $false)]
  [switch]$RequireSnd,

  # If set, the scheduled selftest will skip the virtio-snd section even if a device is present.
  # This adds `--disable-snd` to the scheduled task.
  [Parameter(Mandatory = $false)]
  [switch]$DisableSnd,

  # If set, the scheduled selftest will skip the virtio-snd capture check only (playback still runs).
  # This adds `--disable-snd-capture` to the scheduled task.
  [Parameter(Mandatory = $false)]
  [switch]$DisableSndCapture,

  # If set, run the virtio-snd capture smoke test when a capture endpoint exists
  # (adds `--test-snd-capture` to the scheduled task).
  [Parameter(Mandatory = $false)]
  [switch]$TestSndCapture,

  # If set, fail the overall selftest if no virtio-snd capture endpoint exists
  # (adds `--require-snd-capture` to the scheduled task).
  [Parameter(Mandatory = $false)]
  [switch]$RequireSndCapture,

  # If set, fail the capture smoke test if only silence is captured
  # (adds `--require-non-silence` to the scheduled task).
  [Parameter(Mandatory = $false)]
  [switch]$RequireNonSilence,

  # If set, run the guest selftest's virtio-snd buffer limits stress test
  # (adds `--test-snd-buffer-limits` to the scheduled task; env var alternative:
  # `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`).
  #
  # This is required when running the host harness with `-WithSndBufferLimits` / `--with-snd-buffer-limits`.
  [Parameter(Mandatory = $false)]
  [switch]$TestSndBufferLimits,

  # If set, accept the transitional virtio-snd PCI ID (`PCI\VEN_1AF4&DEV_1018`) in the guest selftest.
  # This adds `--allow-virtio-snd-transitional` to the scheduled task.
  #
  # Note: The host harness's virtio-snd test path expects a modern-only virtio-snd device (PCI\VEN_1AF4&DEV_1059&REV_01).
  # This flag is intended for debugging/backcompat when running the guest selftest outside the strict harness setup.
  [Parameter(Mandatory = $false)]
  [switch]$AllowVirtioSndTransitional,

  # If set, enable the guest selftest's end-to-end virtio-input event delivery test
  # (adds `--test-input-events` to the scheduled task).
  #
  # This is required when running the host harness with `-WithInputEvents` / `--with-input-events`.
  [Parameter(Mandatory = $false)]
  [switch]$TestInputEvents,

  # If set, enable the guest selftest's virtio-input keyboard LED/statusq smoke test
  # (adds `--test-input-leds` to the scheduled task).
  #
  # This is required when running the host harness with `-WithInputLeds` / `--with-input-leds`.
  [Parameter(Mandatory = $false)]
  [switch]$TestInputLeds,
 
  # If set, enable additional virtio-input end-to-end markers for modifiers/buttons/wheel
  # (adds `--test-input-events-extended` to the scheduled task).
  #
  # This is required when running the host harness with:
  # - PowerShell: `-WithInputEventsExtended` / `-WithInputEventsExtra`
  # - Python: `--with-input-events-extended` / `--with-input-events-extra`
  #
  # Note: This implies -TestInputEvents (base `--test-input-events`).
  [Parameter(Mandatory = $false)]
  [Alias("TestInputEventsExtra")]
  [switch]$TestInputEventsExtended,

  # If set, enable the guest selftest's end-to-end virtio-input Consumer Control (media keys) test
  # (adds `--test-input-media-keys` to the scheduled task).
  #
  # This is required when running the host harness with:
  # - PowerShell: `-WithInputMediaKeys`
  # - Python: `--with-input-media-keys`
  [Parameter(Mandatory = $false)]
  [Alias("TestMediaKeys")]
  [switch]$TestInputMediaKeys,

  # If set, enable the guest selftest's virtio-input keyboard LED/statusq smoke test
  # (adds `--test-input-led` to the scheduled task).
  #
  # This is required when running the host harness with:
  # - PowerShell: `-WithInputLed`
  # - Python: `--with-input-led`
  [Parameter(Mandatory = $false)]
  [switch]$TestInputLed,

  # If set, enable the guest selftest's end-to-end virtio-input tablet (absolute pointer) event delivery test
  # (adds `--test-input-tablet-events` (alias: `--test-tablet-events`) to the scheduled task).
  #
  # This is required when running the host harness with:
  # - PowerShell: `-WithInputTabletEvents` / `-WithTabletEvents`
  # - Python: `--with-input-tablet-events` / `--with-tablet-events`
  [Parameter(Mandatory = $false)]
  [Alias("TestTabletEvents")]
  [switch]$TestInputTabletEvents,

  # If set, enable the guest selftest's virtio-blk miniport reset/recovery test
  # (adds `--test-blk-reset` to the scheduled task).
  #
  # This is required when running the host harness with `-WithBlkReset` / `--with-blk-reset`.
  [Parameter(Mandatory = $false)]
  [Alias("TestVirtioBlkReset")]
  [switch]$TestBlkReset,

  # If set, enable virtio-input "CompatIdName" mode in the guest by writing:
  #   HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters\CompatIdName = 1 (REG_DWORD)
  #
  # This relaxes strict Aero contract ID_NAME/ID_DEVIDS validation so the driver can bind to
  # stock QEMU virtio-input devices (which typically report ID_NAME strings like "QEMU Virtio Keyboard").
  #
  # This is useful for the in-tree QEMU host harness, but should be left disabled when validating
  # strict Aero contract v1 device-model conformance.
  #
  # When this flag is NOT set, the generated guest provisioning script leaves CompatIdName unchanged (strict mode).
  [Parameter(Mandatory = $false)]
  [Alias("VirtioInputCompatIdName", "EnableVirtioInputCompat")]
  [switch]$EnableVirtioInputCompatIdName,

  # If set, enable the guest selftest's virtio-net link flap regression test
  # (adds `--test-net-link-flap` to the scheduled task).
  #
  # This is required when running the host harness with `-WithNetLinkFlap` / `--with-net-link-flap`.
  [Parameter(Mandatory = $false)]
  [switch]$TestNetLinkFlap
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# The host harness (`Invoke-AeroVirtioWin7Tests*`) requires the guest selftest to emit PASS markers
# for virtio-snd playback + capture + duplex when `--with-virtio-snd` / `-WithVirtioSnd` is enabled.
#
# Older `aero-virtio-selftest.exe` builds only run the capture + duplex smoke tests when explicitly
# enabled via `--test-snd-capture` (or `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`). Newer selftest
# builds auto-enable capture/duplex whenever a virtio-snd device is present.
#
# To keep provisioning media compatible with both behaviors, default `-TestSndCapture` to on when
# virtio-snd is being required/tested, unless the caller explicitly disabled capture.
if (-not $TestSndCapture -and -not $DisableSnd -and -not $DisableSndCapture -and ($RequireSnd -or $RequireSndMsix -or $RequireSndCapture -or $RequireNonSilence -or $TestSndBufferLimits)) {
  $TestSndCapture = $true
}

function Write-TextFileUtf8NoBom {
  param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$Content)
  $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
  [System.IO.File]::WriteAllText($Path, $Content, $utf8NoBom)
}

$dirSepChars = @([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)

function Get-PathRelativeToBase {
  param(
    [Parameter(Mandatory = $true)][string]$BaseDir,
    [Parameter(Mandatory = $true)][string]$ChildPath
  )
  $baseFull = (Resolve-Path -LiteralPath $BaseDir).Path
  $childFull = (Resolve-Path -LiteralPath $ChildPath).Path

  $basePrefix = $baseFull.TrimEnd($dirSepChars) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $childFull.StartsWith($basePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path '$childFull' is not under base directory '$baseFull'."
  }

  return $childFull.Substring($basePrefix.Length)
}

$SelftestExePath = (Resolve-Path -LiteralPath $SelftestExePath).Path
if (Test-Path -LiteralPath $SelftestExePath -PathType Container) {
  throw "-SelftestExePath must be a file path (got a directory): $SelftestExePath"
}
$DriversDir = (Resolve-Path -LiteralPath $DriversDir).Path
if (-not (Test-Path -LiteralPath $DriversDir -PathType Container)) {
  throw "-DriversDir must be a directory path (got a file): $DriversDir"
}

if (Test-Path -LiteralPath $OutputDir -PathType Leaf) {
  throw "-OutputDir must be a directory path (got a file): $OutputDir"
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null

$markerPath = Join-Path $OutputDir "AERO_PROVISIONING_MEDIA.TXT"
Write-TextFileUtf8NoBom -Path $markerPath -Content @"
This media was generated by drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1

It is intended to be attached to a Windows 7 VM and used to provision:
- Aero virtio drivers
- aero-virtio-selftest.exe
- Task Scheduler auto-run at boot (SYSTEM)
"@

$selftestDir = Join-Path $OutputDir "AERO/selftest"
$driversOutDir = Join-Path $OutputDir "AERO/drivers"
$provisionDir = Join-Path $OutputDir "AERO/provision"

New-Item -ItemType Directory -Path $selftestDir -Force | Out-Null
New-Item -ItemType Directory -Path $driversOutDir -Force | Out-Null
New-Item -ItemType Directory -Path $provisionDir -Force | Out-Null

Copy-Item -LiteralPath $SelftestExePath -Destination (Join-Path $selftestDir "aero-virtio-selftest.exe") -Force
Copy-Item -LiteralPath $DriversDir -Destination $driversOutDir -Recurse -Force

if ($InstallAllInfs -and $PSBoundParameters.ContainsKey("InfAllowList") -and $InfAllowList.Count -gt 0) {
  throw "Do not specify -InfAllowList together with -InstallAllInfs. Use one mode or the other."
}

$driversOutDirResolved = (Resolve-Path -LiteralPath $driversOutDir).Path
# Wrap in @() so $infFiles is always an array (not $null) under strict mode when there are no matches.
$infFiles = @(
  Get-ChildItem -LiteralPath $driversOutDirResolved -Recurse -Filter "*.inf" -File | Sort-Object FullName
)
if ($infFiles.Count -eq 0) {
  throw "No .inf files found under -DriversDir '$DriversDir'."
}

$driversDirLeaf = Split-Path -Leaf $DriversDir

$infIndex = foreach ($inf in $infFiles) {
  $rel = Get-PathRelativeToBase -BaseDir $driversOutDirResolved -ChildPath $inf.FullName
  $relWin = ($rel -replace "/", "\")

  $relWinNoLeaf = $relWin
  if (-not [string]::IsNullOrEmpty($driversDirLeaf)) {
    $leafPrefix = $driversDirLeaf + "\"
    if ($relWinNoLeaf.StartsWith($leafPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
      $relWinNoLeaf = $relWinNoLeaf.Substring($leafPrefix.Length)
    }
  }

  [pscustomobject]@{
    FullPath = $inf.FullName
    Name = $inf.Name
    RelPathWin = $relWin
    RelPathWinNoLeaf = $relWinNoLeaf
  }
}

$defaultInfAllowList = @(
  "aero_virtio_blk.inf",
  "aero_virtio_net.inf",
  "aero_virtio_input.inf",
  "aero_virtio_tablet.inf",
  "aero_virtio_snd.inf"
)

if ($AllowVirtioSndTransitional) {
  # When we allow the transitional virtio-snd PCI ID in the guest selftest, also stage the optional
  # transitional driver package by default (if present in the provided drivers directory).
  # This is a no-op in strict contract-v1 setups since the legacy INF matches only PCI\VEN_1AF4&DEV_1018.
  $defaultInfAllowList += "aero-virtio-snd-legacy.inf"
}

$installDriversCmd = ""
$readmeDriverInstallDesc = ""
if ($InstallAllInfs) {
  $installDriversCmd = @"
REM Install drivers (legacy mode: all .inf files under AERO\drivers).
echo [AERO] installing drivers (InstallAllInfs)... >> "%LOG%"
for /r "%MEDIA%\AERO\drivers" %%F in (*.inf) do (
  echo [AERO] pnputil -i -a "%%F" >> "%LOG%"
  pnputil -i -a "%%F" >> "%LOG%" 2>&1
  if errorlevel 1 (
    echo [AERO] ERROR: pnputil failed for "%%F" >> "%LOG%"
    exit /b 1
  )
)
"@
  $readmeDriverInstallDesc = "Install ALL driver .inf files under AERO\drivers via pnputil (InstallAllInfs mode)"
} else {
  $effectiveAllowList = @()
  $allowListSource = ""
  if ($PSBoundParameters.ContainsKey("InfAllowList")) {
    if ($InfAllowList.Count -eq 0) {
      throw "-InfAllowList was provided but is empty. Provide at least one INF or omit the parameter."
    }
    $effectiveAllowList = $InfAllowList
    $allowListSource = "user allowlist"
  } else {
    $effectiveAllowList = $defaultInfAllowList
    $allowListSource = "default allowlist"
  }

  $resolvedInfRelPaths = New-Object System.Collections.Generic.List[string]
  $resolvedRelPathSet = @{}

  foreach ($entry in $effectiveAllowList) {
    $entryNorm = ([string]$entry).Trim()
    if ([string]::IsNullOrEmpty($entryNorm)) {
      throw "InfAllowList contains an empty entry."
    }

    $entryWin = ($entryNorm -replace "/", "\").TrimStart("\")
    if ($entryWin.StartsWith(".\", [System.StringComparison]::OrdinalIgnoreCase)) {
      $entryWin = $entryWin.Substring(2)
    }
    $isRelative = $entryWin.Contains("\")

    $matches = @()
    if ($isRelative) {
      # Wrap in @() so $matches is always an array (not $null) under strict mode when there are no results.
      $matches = @(
        $infIndex | Where-Object { $_.RelPathWinNoLeaf -ieq $entryWin -or $_.RelPathWin -ieq $entryWin }
      )
    } else {
      $matches = @(
        $infIndex | Where-Object { $_.Name -ieq $entryWin }
      )
    }

    if ($matches.Count -eq 0) {
      if ($allowListSource -eq "default allowlist") {
        Write-Warning "Default allowlisted INF '$entryNorm' was not found under -DriversDir '$DriversDir' and will be skipped."
        continue
      }

      $available = ($infIndex | Select-Object -ExpandProperty RelPathWin | Sort-Object) -join ", "
      throw "InfAllowList entry '$entryNorm' did not match any .inf under -DriversDir '$DriversDir'. Available INFs: $available"
    }

    if ($matches.Count -gt 1 -and -not $isRelative -and $allowListSource -eq "default allowlist") {
      # If multiple INFs share the same basename (common when staging driver packs),
      # prefer the contract-v1 IDs used by the harness (`disable-legacy=on,x-pci-revision=0x01`).
      # If still ambiguous after filtering, require an explicit relative path.
      $preferPattern = $null
      switch ($entryWin.ToLowerInvariant()) {
        "aero_virtio_blk.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1042&REV_01" }
        "aero_virtio_net.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1041&REV_01" }
        # Prefer a SUBSYS-qualified contract HWID for disambiguation (more specific than the generic fallback).
        "aero_virtio_input.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01" }
        "aero_virtio_tablet.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01" }
        "aero_virtio_snd.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1059&REV_01" }
        "aero-virtio-snd-legacy.inf" { $preferPattern = "PCI\VEN_1AF4&DEV_1018" }
      }

      if ($preferPattern) {
        $preferredMatches = @(
          $matches | Where-Object { Select-String -LiteralPath $_.FullPath -Pattern $preferPattern -SimpleMatch -Quiet }
        )
        if ($preferredMatches.Count -eq 1) {
          $matches = $preferredMatches
        }
      }
    }

    if ($matches.Count -gt 1) {
      $ambiguous = ($matches | Select-Object -ExpandProperty RelPathWin | Sort-Object) -join ", "
      throw "InfAllowList entry '$entryNorm' matched multiple INFs under -DriversDir '$DriversDir': $ambiguous. Use a relative path (e.g. 'subdir\\driver.inf') to disambiguate."
    }

    $relPathWin = $matches[0].RelPathWin
    if (-not $resolvedRelPathSet.ContainsKey($relPathWin.ToLowerInvariant())) {
      $resolvedRelPathSet[$relPathWin.ToLowerInvariant()] = $true
      $resolvedInfRelPaths.Add($relPathWin)
    }
  }

  if ($resolvedInfRelPaths.Count -eq 0) {
    throw "No allowed INF files resolved from $allowListSource. Pass -InfAllowList or use -InstallAllInfs."
  }

  $resolvedListStr = ($resolvedInfRelPaths | Sort-Object) -join ", "
  Write-Host "Driver install mode: $allowListSource"
  Write-Host "Will install INF(s): $resolvedListStr"
  $readmeDriverInstallDesc = "Install allowlisted driver .inf files via pnputil ($allowListSource): $resolvedListStr"

  # Wrap in @() so $ignored is always an array (not $null) under strict mode when there are no results.
  $ignored = @(
    $infIndex | Where-Object { -not $resolvedRelPathSet.ContainsKey($_.RelPathWin.ToLowerInvariant()) }
  )
  if ($ignored.Count -gt 0) {
    $ignoredStr = ($ignored | Select-Object -ExpandProperty RelPathWin | Sort-Object) -join ", "
    Write-Warning "The following INF(s) are present under -DriversDir but will NOT be installed unless allowlisted: $ignoredStr"
  }

  $installBlocks = New-Object System.Collections.Generic.List[string]
  foreach ($relPathWin in $resolvedInfRelPaths) {
    $infMediaPath = "%MEDIA%\AERO\drivers\$relPathWin"
    $installBlocks.Add(@"
if not exist "$infMediaPath" (
  echo [AERO] ERROR: allowed INF not found: "$infMediaPath" >> "%LOG%"
  exit /b 1
)
echo [AERO] pnputil -i -a "$infMediaPath" >> "%LOG%"
pnputil -i -a "$infMediaPath" >> "%LOG%" 2>&1
if errorlevel 1 (
  echo [AERO] ERROR: pnputil failed for "$infMediaPath" >> "%LOG%"
  exit /b 1
)
"@
    )
  }

  $installDriversCmd = @"
REM Install drivers (INF allowlist).
echo [AERO] installing drivers (allowlist)... >> "%LOG%"
$($installBlocks -join "`r`n")
"@
}

$blkArg = ""
if (-not [string]::IsNullOrEmpty($BlkRoot)) {
  # schtasks /TR quoting: use backslash-escaped quotes (\"...\") so paths with spaces are safe.
  #
  # Note: Windows command-line parsing treats a backslash before a closing quote specially. If the
  # caller provides a BlkRoot that ends with one or more backslashes (common for directory paths,
  # e.g. "D:\aero-test\"), those backslashes can escape the closing quote and break parsing.
  #
  # We need to handle two layers of parsing:
  #  1) `schtasks /Create ... /TR "<cmdline>"` must parse the /TR argument correctly.
  #  2) Later, when the scheduled task runs, Windows parses the stored command line into argv for
  #     `aero-virtio-selftest.exe`.
  #
  # To ensure the stored /TR command line contains the required *doubled* trailing backslashes (so
  # the selftest sees the intended `\`), we must expand the caller's trailing backslashes by 4x here.
  $blkRootEscaped = [regex]::Replace($BlkRoot, "\\+$", { param($m) $m.Value + $m.Value + $m.Value + $m.Value })
  $blkArg = " --blk-root " + '\"' + $blkRootEscaped + '\"'
}

$udpArg = ""
if ($PSBoundParameters.ContainsKey("UdpPort")) {
  $udpArg = " --udp-port $UdpPort"
}

$expectBlkMsiArg = ""
if ($ExpectBlkMsi) {
  $expectBlkMsiArg = " --expect-blk-msi"
}

$requireNetMsixArg = ""
if ($RequireNetMsix) {
  $requireNetMsixArg = " --require-net-msix"
}

$requireInputMsixArg = ""
if ($RequireInputMsix) {
  $requireInputMsixArg = " --require-input-msix"
}

$requireSndMsixArg = ""
if ($RequireSndMsix) {
  $requireSndMsixArg = " --require-snd-msix"
}

if ($RequireSnd -and $DisableSnd) {
  throw "RequireSnd and DisableSnd cannot both be set."
}
if ($RequireSndMsix -and $DisableSnd) {
  throw "RequireSndMsix and DisableSnd cannot both be set."
}
if ($DisableSnd -and ($TestSndCapture -or $RequireSndCapture -or $RequireNonSilence -or $TestSndBufferLimits)) {
  throw "DisableSnd cannot be combined with TestSndCapture/RequireSndCapture/RequireNonSilence/TestSndBufferLimits."
}
if ($DisableSndCapture -and ($TestSndCapture -or $RequireSndCapture -or $RequireNonSilence)) {
  throw "DisableSndCapture cannot be combined with TestSndCapture/RequireSndCapture/RequireNonSilence."
}
if ($DisableSnd -and $AllowVirtioSndTransitional) {
  throw "DisableSnd cannot be combined with AllowVirtioSndTransitional."
}

$requireSndArg = ""
if ($RequireSnd) {
  $requireSndArg = " --require-snd"
}

$disableSndArg = ""
if ($DisableSnd) {
  $disableSndArg = " --disable-snd"
}

$disableSndCaptureArg = ""
if ($DisableSndCapture -and -not $DisableSnd) {
  $disableSndCaptureArg = " --disable-snd-capture"
}

$testSndCaptureArg = ""
if ($TestSndCapture) {
  $testSndCaptureArg = " --test-snd-capture"
}

$requireSndCaptureArg = ""
if ($RequireSndCapture) {
  $requireSndCaptureArg = " --require-snd-capture"
}

$requireNonSilenceArg = ""
if ($RequireNonSilence) {
  $requireNonSilenceArg = " --require-non-silence"
}

$testSndBufferLimitsArg = ""
if ($TestSndBufferLimits) {
  $testSndBufferLimitsArg = " --test-snd-buffer-limits"
}

$allowVirtioSndTransitionalArg = ""
if ($AllowVirtioSndTransitional) {
  $allowVirtioSndTransitionalArg = " --allow-virtio-snd-transitional"
}

$testBlkResizeArg = ""
if ($TestBlkResize) {
  $testBlkResizeArg = " --test-blk-resize"
}

$testInputEventsArg = ""
if ($TestInputEvents -or $TestInputEventsExtended) {
  $testInputEventsArg = " --test-input-events"
}

$testInputEventsExtendedArg = ""
if ($TestInputEventsExtended) {
  $testInputEventsExtendedArg = " --test-input-events-extended"
}

$testInputMediaKeysArg = ""
if ($TestInputMediaKeys) {
  $testInputMediaKeysArg = " --test-input-media-keys"
}

$testInputLedArg = ""
if ($TestInputLed) {
  $testInputLedArg = " --test-input-led"
}

$testInputLedsArg = ""
if ($TestInputLeds) {
  $testInputLedsArg = " --test-input-leds"
}

$testInputTabletEventsArg = ""
if ($TestInputTabletEvents) {
  $testInputTabletEventsArg = " --test-input-tablet-events"
}

$testBlkResetArg = ""
if ($TestBlkReset) {
  $testBlkResetArg = " --test-blk-reset"
}

$testNetLinkFlapArg = ""
if ($TestNetLinkFlap) {
  $testNetLinkFlapArg = " --test-net-link-flap"
}

$enableTestSigningCmd = ""
if ($EnableTestSigning) {
  $enableTestSigningCmd = @"
REM Enable Windows test-signing mode (required for unsigned/test-signed kernel drivers on Win7 x64).
echo [AERO] enabling testsigning... >> "%LOG%"
bcdedit /set testsigning on >> "%LOG%" 2>&1
"@
}

$autoRebootCmd = ""
if ($AutoReboot) {
  $autoRebootCmd = @"
echo [AERO] rebooting... >> "%LOG%"
shutdown /r /t 0 >> "%LOG%" 2>&1
"@
}

$enableVirtioInputCompatCmd = ""
if ($EnableVirtioInputCompatIdName) {
  $enableVirtioInputCompatCmd = @"

REM Enable virtio-input CompatIdName mode (accept QEMU ID_NAME strings, relax ID_DEVIDS checks).
echo [AERO] enabling virtio-input CompatIdName=1 >> "%LOG%"
reg add "HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters" /v CompatIdName /t REG_DWORD /d 1 /f >> "%LOG%" 2>&1
if errorlevel 1 (
  echo [AERO] ERROR: failed to set aero_virtio_input CompatIdName registry flag >> "%LOG%"
  exit /b 1
)
"@
}

$provisionCmd = @"
@echo off
setlocal enableextensions enabledelayedexpansion

set LOG=C:\aero-win7-provision.log
echo [AERO] provision start > "%LOG%"

REM Locate the provisioning media by searching drive letters for the marker file.
set MEDIA=
for %%D in (D E F G H I J K L M N O P Q R S T U V W X Y Z) do (
  if exist %%D:\AERO_PROVISIONING_MEDIA.TXT set MEDIA=%%D:
)

if "%MEDIA%"=="" (
  echo [AERO] ERROR: provisioning media not found >> "%LOG%"
  exit /b 1
)

echo [AERO] MEDIA=%MEDIA% >> "%LOG%"

$installDriversCmd
$enableVirtioInputCompatCmd

REM Install selftest binary.
mkdir C:\AeroTests >> "%LOG%" 2>&1
copy /y "%MEDIA%\AERO\selftest\aero-virtio-selftest.exe" C:\AeroTests\ >> "%LOG%" 2>&1

$enableTestSigningCmd

REM Configure auto-run on boot (runs as SYSTEM).
schtasks /Create /F /TN "AeroVirtioSelftest" /SC ONSTART /RU SYSTEM ^
  /TR "\"C:\AeroTests\aero-virtio-selftest.exe\" --http-url \"$HttpUrl\" --dns-host \"$DnsHost\"$udpArg$requireNetMsixArg$requireInputMsixArg$requireSndMsixArg$blkArg$expectBlkMsiArg$testBlkResizeArg$testBlkResetArg$testInputEventsArg$testInputEventsExtendedArg$testInputMediaKeysArg$testInputLedsArg$testInputLedArg$testInputTabletEventsArg$testNetLinkFlapArg$requireSndArg$disableSndArg$disableSndCaptureArg$testSndCaptureArg$requireSndCaptureArg$requireNonSilenceArg$testSndBufferLimitsArg$allowVirtioSndTransitionalArg" >> "%LOG%" 2>&1

echo [AERO] provision done >> "%LOG%"
$autoRebootCmd
exit /b 0
"@

Write-TextFileUtf8NoBom -Path (Join-Path $provisionDir "provision.cmd") -Content $provisionCmd

$readmeVirtioInputCompatProvisionDesc = ""
$readmeVirtioInputCompatNotes = ""
if ($EnableVirtioInputCompatIdName) {
  $readmeVirtioInputCompatProvisionDesc = @"
- Enable virtio-input ID_NAME compatibility for stock QEMU virtio-input devices by setting:
  HKLM\SYSTEM\CurrentControlSet\Services\aero_virtio_input\Parameters\CompatIdName=1
"@

  $readmeVirtioInputCompatNotes = @"
  - Stock QEMU virtio-input devices typically report non-Aero `ID_NAME` strings (for example `QEMU Virtio Keyboard`).
    The Aero virtio-input driver defaults to strict contract mode and may refuse to start (Code 10) unless compatibility mode is enabled.
    - This media enables QEMU compatibility mode via `-EnableVirtioInputCompatIdName` (alias: `-EnableVirtioInputCompat`).
"@
}

$readme = @"
Provisioning instructions (Windows 7 guest)
===========================================

This media contains:
- AERO\selftest\aero-virtio-selftest.exe
- AERO\drivers\... (driver .inf files)
- AERO\provision\provision.cmd

To provision an already-installed Windows 7 image:
1) Boot the VM (any disk/NIC is fine for this step).
2) Attach this media as a CD-ROM.
3) Run (as Administrator):

   <CD>:\AERO\provision\provision.cmd

The script will:
- $readmeDriverInstallDesc
$readmeVirtioInputCompatProvisionDesc
- Copy the selftest to C:\AeroTests\
- Create a scheduled task (SYSTEM, ONSTART) that runs the selftest each boot.

After reboot, the host harness can boot the VM and parse PASS/FAIL from COM1 serial.

  Notes:
  - The virtio-blk selftest requires a usable/mounted virtio volume. If your VM boots from a non-virtio disk,
    consider attaching a separate virtio data disk with a drive letter and using the selftest option `--blk-root`.
  - To fail the virtio-blk selftest when the driver is still using INTx (expected MSI/MSI-X), generate this media with
    `-ExpectBlkMsi` (adds `--expect-blk-msi` to the scheduled task). This is the guest-side complement to the host harness flags
    `-RequireMsi` / `--require-msi` (which enforce MSI/MSI-X based on virtio-*-irq markers).
  - To fail the virtio-net selftest when the driver is not using MSI-X (expected MSI-X), generate this media with
    `-RequireNetMsix` (adds `--require-net-msix` to the scheduled task). This is the guest-side complement to the host harness flag
    `-RequireVirtioNetMsix` / `--require-virtio-net-msix` (which also performs a host-side MSI-X enable check via QMP).
  - To fail the virtio-input selftest when the driver is not using MSI-X (expected MSI-X), generate this media with
    `-RequireInputMsix` (adds `--require-input-msix` to the scheduled task). This is the guest-side complement to the host harness flag
    `-RequireVirtioInputMsix` / `--require-virtio-input-msix` (which validates the guest `virtio-input-msix` marker).
  - To fail the virtio-snd selftest when the driver is not using MSI-X (expected MSI-X), generate this media with
    `-RequireSndMsix` (adds `--require-snd-msix` to the scheduled task). This is the guest-side complement to the host harness flag
    `-RequireVirtioSndMsix` / `--require-virtio-snd-msix` (which also performs a host-side MSI-X enable check via QMP).
    When enabled, the guest selftest also fails if the virtio-snd device is missing or the diagnostics interface is unavailable.
  - The virtio-blk runtime resize test (`virtio-blk-resize`) is disabled by default (requires host-side QMP resize).
    - To enable it (required when running the host harness with `-WithBlkResize` / `--with-blk-resize`),
      generate this media with `-TestBlkResize` (adds `--test-blk-resize` to the scheduled task).
  - To enable the virtio-blk miniport reset/recovery stability test (required when running the host harness with
    `-WithBlkReset` / `--with-blk-reset`), generate this media with `-TestBlkReset` (adds `--test-blk-reset`).
  - The virtio-net selftest also performs a UDP echo smoke test against the host harness (10.0.2.2:<port>).
    - Default UDP port: 18081 (must match the host harness UDP echo server port).
    - If you need to override the port, regenerate this media with `-UdpPort <port>` (adds `--udp-port` to the scheduled task)
      and run the host harness with the same port.
  - The virtio-input end-to-end event delivery tests (HID input reports) are disabled by default.
    - To enable keyboard/mouse injection (required when running the host harness with `-WithInputEvents` / `--with-input-events`),
      generate this media with `-TestInputEvents` (adds `--test-input-events` to the scheduled task).
    - To also enable the extended virtio-input markers (modifiers/buttons/wheel) (required when running the host harness with
      `-WithInputEventsExtended` / `-WithInputEventsExtra` / `--with-input-events-extended` / `--with-input-events-extra`), generate this media with
      `-TestInputEventsExtended` (alias: `-TestInputEventsExtra`) (adds `--test-input-events-extended` to the scheduled task, and implies `--test-input-events`).
    - To enable Consumer Control / media keys injection (required when running the host harness with `-WithInputMediaKeys` / `--with-input-media-keys`),
      generate this media with `-TestInputMediaKeys` (alias: `-TestMediaKeys`) (adds `--test-input-media-keys` to the scheduled task).
    - To enable the keyboard LED/statusq smoke test (required when running the host harness with
      `-WithInputLed` / `--with-input-led` and/or `-WithInputLeds` / `--with-input-leds`):
      - `-TestInputLed` (adds `--test-input-led` to the scheduled task)
      - `-TestInputLeds` (adds `--test-input-leds` to the scheduled task)
    - To enable tablet (absolute pointer) injection (required when running the host harness with `-WithInputTabletEvents` / `-WithTabletEvents` /
      `--with-input-tablet-events` / `--with-tablet-events`), generate this media with `-TestInputTabletEvents` (alias: `-TestTabletEvents`)
      (adds `--test-input-tablet-events` (alias: `--test-tablet-events`) to the scheduled task).
$readmeVirtioInputCompatNotes
  - The virtio-net link flap regression test is disabled by default.
    - To enable it (required when running the host harness with `-WithNetLinkFlap` / `--with-net-link-flap`),
      generate this media with `-TestNetLinkFlap` (adds `--test-net-link-flap` to the scheduled task).
  - By default, virtio-snd is optional (SKIP if missing). To require it, generate this media with `-RequireSnd` (adds `--require-snd`).
    - To skip the virtio-snd test entirely, generate this media with `-DisableSnd`.
      Note: if you run the host harness with `-WithVirtioSnd` / `--with-virtio-snd`, it expects virtio-snd to PASS (not SKIP).
    - To skip capture-only checks (while still exercising playback), generate this media with `-DisableSndCapture` (adds `--disable-snd-capture`).
      Note: if you run the host harness with `-WithVirtioSnd` / `--with-virtio-snd`, it expects virtio-snd-capture to PASS (not SKIP).
   - To run the virtio-snd capture smoke test (and enable the full-duplex regression test):
      - Newer `aero-virtio-selftest.exe` binaries run capture/duplex automatically whenever virtio-snd is present.
      - For older selftest binaries, generate this media with `-TestSndCapture` (adds `--test-snd-capture`; env var equivalent:
        `AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1`). This script also
        defaults `-TestSndCapture` on when virtio-snd is being required/tested, unless capture is explicitly disabled.
        - Use `-RequireSndCapture` to fail if no capture endpoint exists.
        - Use `-RequireNonSilence` to fail if only silence is captured.
   - To run the virtio-snd buffer limits stress test (required when running the host harness with
      `-WithSndBufferLimits` / `--with-snd-buffer-limits`), generate this media with `-TestSndBufferLimits`
      (adds `--test-snd-buffer-limits` to the scheduled task; env var equivalent: `AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1`).
  - To accept the transitional virtio-snd PCI ID (`PCI\VEN_1AF4&DEV_1018`) in the guest selftest, generate this media with
    `-AllowVirtioSndTransitional` (adds `--allow-virtio-snd-transitional`).
  - For unsigned/test-signed drivers on Win7 x64, consider generating this media with `-EnableTestSigning -AutoReboot`.
"@

Write-TextFileUtf8NoBom -Path (Join-Path $OutputDir "README.txt") -Content $readme

Write-Host "Generated provisioning directory: $OutputDir"

if (-not [string]::IsNullOrEmpty($OutputIsoPath)) {
  $isoParent = Split-Path -Parent $OutputIsoPath
  if ([string]::IsNullOrEmpty($isoParent)) { $isoParent = "." }
  if (-not (Test-Path -LiteralPath $isoParent)) {
    New-Item -ItemType Directory -Path $isoParent -Force | Out-Null
  }
  $OutputIsoPath = Join-Path (Resolve-Path -LiteralPath $isoParent).Path (Split-Path -Leaf $OutputIsoPath)
  if (Test-Path -LiteralPath $OutputIsoPath -PathType Container) {
    throw "-OutputIsoPath must be a file path (got a directory): $OutputIsoPath"
  }

  $oscdimg = Get-Command oscdimg -ErrorAction SilentlyContinue
  $mkisofs = Get-Command mkisofs -ErrorAction SilentlyContinue
  $genisoimage = Get-Command genisoimage -ErrorAction SilentlyContinue

  if ($oscdimg) {
    Write-Host "Building ISO via oscdimg: $OutputIsoPath"
    & $oscdimg.Source "-n" "-m" $OutputDir $OutputIsoPath
  } elseif ($mkisofs) {
    Write-Host "Building ISO via mkisofs: $OutputIsoPath"
    & $mkisofs.Source "-quiet" "-o" $OutputIsoPath "-J" "-r" $OutputDir
  } elseif ($genisoimage) {
    Write-Host "Building ISO via genisoimage: $OutputIsoPath"
    & $genisoimage.Source "-quiet" "-o" $OutputIsoPath "-J" "-r" $OutputDir
  } else {
    Write-Warning "OutputIsoPath specified, but no ISO tool found (oscdimg/mkisofs/genisoimage)."
    Write-Warning "You can still attach the directory contents via your preferred ISO creation tool."
  }
}
