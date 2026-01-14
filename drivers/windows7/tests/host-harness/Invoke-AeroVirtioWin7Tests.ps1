# SPDX-License-Identifier: MIT OR Apache-2.0

[CmdletBinding()]
param(
  # QEMU system binary (e.g. qemu-system-x86_64)
  [Parameter(Mandatory = $true)]
  [string]$QemuSystem,

  # Windows 7 disk image that is already installed + provisioned to run the selftest at boot.
  [Parameter(Mandatory = $true)]
  [string]$DiskImagePath,

  # Where to write the captured COM1 serial output.
  [Parameter(Mandatory = $false)]
  [string]$SerialLogPath = "./win7-virtio-serial.log",

  [Parameter(Mandatory = $false)]
  [int]$MemoryMB = 2048,

  [Parameter(Mandatory = $false)]
  [int]$Smp = 2,

  # If set, run QEMU in snapshot mode for the main disk (writes are discarded on exit).
  [Parameter(Mandatory = $false)]
  [switch]$Snapshot,

  # If set, use QEMU's transitional virtio-pci devices (legacy + modern).
  # By default this harness uses modern-only (disable-legacy=on) virtio-pci devices so
  # Win7 drivers can bind to the Aero contract v1 IDs (DEV_1041/DEV_1042/DEV_1052/DEV_1059) and
  # revision gate (REV_01).
  #
  # Transitional mode is primarily a backcompat option for older QEMU builds and/or older guest images:
  # - It uses QEMU defaults for virtio-blk/net and relaxes per-test marker requirements.
  # - It attempts to attach virtio-input keyboard/mouse devices (virtio-keyboard-pci + virtio-mouse-pci) when
  #   the QEMU binary advertises them; otherwise it warns that the guest virtio-input selftest will likely FAIL.
  #   In transitional mode, virtio-input may enumerate with the older transitional ID space (e.g. DEV_1011)
  #   depending on QEMU, so the guest must have a driver package that binds the IDs your QEMU build exposes.
  [Parameter(Mandatory = $false)]
  [switch]$VirtioTransitional,

  # Optional MSI-X vector count to request from virtio-pci devices via the QEMU
  # `vectors=` device property (when supported by the running QEMU build).
  #
  # When set (> 0), the harness appends `,vectors=<N>` to the virtio-net/blk/input/snd
  # `-device` args that it creates.
  #
  # This knob is best-effort: the harness probes whether each QEMU device advertises the
  # `vectors` property (via `-device <name>,help`). If unsupported, it warns and runs
  # without the override.
  #
  # Typical values: 2, 4, 8. Windows may still allocate fewer MSI-X messages than
  # requested; the Aero drivers are expected to fall back to the number of vectors
  # actually granted (including single-vector MSI-X or INTx).
  [Parameter(Mandatory = $false)]
  [int]$VirtioMsixVectors = 0,

  # Optional per-device MSI-X vector overrides. When set (> 0), these override -VirtioMsixVectors for the
  # corresponding device class.
  #
  # These knobs are best-effort: QEMU must support the `vectors` device property for the target device.
  [Parameter(Mandatory = $false)]
  [Alias("VirtioNetMsixVectors")]
  [int]$VirtioNetVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioBlkMsixVectors")]
  [int]$VirtioBlkVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioSndMsixVectors")]
  [int]$VirtioSndVectors = 0,

  [Parameter(Mandatory = $false)]
  [Alias("VirtioInputMsixVectors")]
  [int]$VirtioInputVectors = 0,

  # If set, require INTx interrupt mode for attached virtio devices (blk/net/input/snd).
  # Fails if the guest reports MSI/MSI-X via virtio-*-irq markers.
  [Parameter(Mandatory = $false)]
  [switch]$RequireIntx,

  # If set, require MSI/MSI-X interrupt mode for attached virtio devices (blk/net/input/snd).
  # Fails if the guest reports INTx via virtio-*-irq markers.
  [Parameter(Mandatory = $false)]
  [switch]$RequireMsi,
 
  # If set, require that the guest selftest was provisioned with `--expect-blk-msi`
  # (i.e. the guest emits `AERO_VIRTIO_SELFTEST|CONFIG|...|expect_blk_msi=1`).
  #
  # This is useful in MSI/MSI-X-specific CI to fail deterministically when running against
  # a mis-provisioned image (where the guest selftest would otherwise accept INTx fallback).
  [Parameter(Mandatory = $false)]
  [switch]$RequireExpectBlkMsi,

  # If set, inject deterministic keyboard/mouse events via QMP (`input-send-event`) and require the guest
  # virtio-input end-to-end event delivery marker (`virtio-input-events`) to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS/FAIL|attempt=<n>|...
  #
  # Note: The guest image must be provisioned with `--test-input-events` (or env var equivalent) so the
  # guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputEvents", "EnableVirtioInputEvents")]
  [switch]$WithInputEvents,

  # If set, inject deterministic Consumer Control (media key) events via QMP (`input-send-event`) and require the guest
  # virtio-input-media-keys marker to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS/FAIL|attempt=<n>|kbd_mode=device/broadcast
  #
  # Note: The guest image must be provisioned with `--test-input-media-keys` (or env var equivalent) so the
  # guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputMediaKeys", "EnableVirtioInputMediaKeys")]
  [switch]$WithInputMediaKeys,

  # If set, also inject vertical + horizontal scroll wheel events (QMP rel axes: wheel + hscroll) and
  # require the guest virtio-input-wheel marker to PASS.
  # This implies -WithInputEvents.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputWheel", "EnableVirtioInputWheel")]
  [switch]$WithInputWheel,

  # If set, also inject and require additional virtio-input end-to-end markers:
  #   - virtio-input-events-modifiers (Shift/Ctrl/Alt + F1)
  #   - virtio-input-events-buttons   (side/extra mouse buttons)
  #   - virtio-input-events-wheel     (mouse wheel)
  #
  # This implies -WithInputEvents, and requires the guest image provisioned with the corresponding
  # guest selftest flags/env vars (e.g. --test-input-events-extended).
  [Parameter(Mandatory = $false)]
  [Alias("WithInputEventsExtra")]
  [switch]$WithInputEventsExtended,

  # If set, attach a virtio-tablet-pci device, inject deterministic absolute-pointer events via QMP (`input-send-event`),
  # and require the guest virtio-input-tablet-events marker to PASS.
  #
  # Also emits a host marker for each injection attempt:
  #   AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS/FAIL|attempt=<n>|tablet_mode=device/broadcast
  #
  # Note: The guest image must be provisioned with `--test-input-tablet-events` (alias: `--test-tablet-events`)
  # (or env var equivalent, e.g. AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1 or
  # AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1) so the guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputTabletEvents", "EnableVirtioInputTabletEvents", "WithTabletEvents", "EnableTabletEvents")]
  [switch]$WithInputTabletEvents,

  # If set, attach a virtio-tablet-pci device in addition to the virtio keyboard/mouse.
  # Unlike -WithInputTabletEvents, this does not inject QMP events or require the guest marker.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioTablet")]
  [switch]$WithVirtioTablet,

  # If set, run a QMP `query-pci` preflight to validate QEMU-emitted virtio PCI Vendor/Device/Revision IDs.
  # In default (contract-v1) mode this enforces VEN_1AF4 + DEV_1041/DEV_1042/DEV_1052[/DEV_1059] and REV_01.
  # In transitional mode this is permissive and only asserts that at least one VEN_1AF4 device exists.
  [Parameter(Mandatory = $false)]
  [Alias("QmpPreflightPci")]
  [switch]$QemuPreflightPci,

  # If set, require that the corresponding virtio PCI function has MSI-X enabled.
  # Verification is performed via QMP/QEMU introspection (query-pci or HMP `info pci` fallback).
  #
  # For virtio-blk, this also requires the guest to report running in MSI-X mode via:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...
  #
  # For virtio-snd, this also requires the guest to report running in MSI-X mode via:
  #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...
  [Parameter(Mandatory = $false)]
  [switch]$RequireVirtioNetMsix,

  [Parameter(Mandatory = $false)]
  [switch]$RequireVirtioBlkMsix,

  [Parameter(Mandatory = $false)]
  [switch]$RequireVirtioSndMsix,

  # If set, require the guest virtio-input-msix marker to report mode=msix.
  # This is optional so older guest selftest binaries (which don't emit the marker) can still run.
  [Parameter(Mandatory = $false)]
  [switch]$RequireVirtioInputMsix,

  # If set, stream newly captured COM1 serial output to stdout while waiting.
  [Parameter(Mandatory = $false)]
  [switch]$FollowSerial,

  [Parameter(Mandatory = $false)]
  [int]$TimeoutSeconds = 600,

  # HTTP server port on the host.
  # Guest reaches it at:
  #   http://10.0.2.2:<port>/aero-virtio-selftest
  # and the virtio-net selftest also fetches:
  #   http://10.0.2.2:<port>/aero-virtio-selftest-large
  [Parameter(Mandatory = $false)]
  [int]$HttpPort = 18080,

  [Parameter(Mandatory = $false)]
  # Base HTTP path. The harness also serves a deterministic 1 MiB payload at "${HttpPath}-large".
  [string]$HttpPath = "/aero-virtio-selftest",

  # UDP echo server port on the host (loopback).
  # Guest reaches it at:
  #   10.0.2.2:<port>
  [Parameter(Mandatory = $false)]
  [int]$UdpPort = 18081,

  # If set, do not start the host UDP echo server and do not require the guest virtio-net-udp marker.
  # This is useful when running the harness against older guest selftest binaries that do not yet
  # implement the UDP test.
  [Parameter(Mandatory = $false)]
  [switch]$DisableUdp,

  # If set, attach a virtio-snd device (virtio-sound-pci / virtio-snd-pci).
  # Note: the guest selftest always emits virtio-snd markers (playback + capture + duplex), but will report SKIP if the
  # virtio-snd PCI device is missing or the test was disabled. When -WithVirtioSnd is enabled, the harness
  # requires virtio-snd, virtio-snd-capture, and virtio-snd-duplex to PASS.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioSnd")]
  [switch]$WithVirtioSnd,

  # If set, require the guest virtio-snd-buffer-limits marker to PASS.
  #
  # Note: this requires:
  # - a guest image provisioned with `--test-snd-buffer-limits` (for example via New-AeroWin7TestImage.ps1 -TestSndBufferLimits), and
  # - -WithVirtioSnd (so a virtio-snd device is attached).
  [Parameter(Mandatory = $false)]
  [switch]$WithSndBufferLimits,

  # NOTE: `-WithVirtioInputEvents` / `-EnableVirtioInputEvents` are accepted as aliases for `-WithInputEvents`
  # for backwards compatibility.

  # Audio backend for virtio-snd.
  # - none: no host audio (device exists)
  # - wav:  capture deterministic audio output to a wav file
  [Parameter(Mandatory = $false)]
  [ValidateSet("none", "wav")]
  [string]$VirtioSndAudioBackend = "none",

  # Output wav path when VirtioSndAudioBackend is "wav".
  [Parameter(Mandatory = $false)]
  [string]$VirtioSndWavPath = "",

  # If set, verify that the virtio-snd wav capture contains non-silent PCM audio.
  # This closes the loop between guest-side playback success and host-side audio backend output.
  [Parameter(Mandatory = $false)]
  [switch]$VerifyVirtioSndWav,

  # Peak absolute sample threshold for VerifyVirtioSndWav (16-bit PCM units; used as a reference scale).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavPeakThreshold = 200,

  # RMS threshold for VerifyVirtioSndWav (16-bit PCM units; used as a reference scale).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavRmsThreshold = 50,

  # Extra args passed verbatim to QEMU (advanced use).
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "AeroVirtioWin7QemuArgs.ps1")

# Stable QOM `id=` values for virtio-input devices so QMP can target them explicitly.
$script:VirtioInputKeyboardQmpId = "aero_virtio_kbd0"
$script:VirtioInputMouseQmpId = "aero_virtio_mouse0"
$script:VirtioInputTabletQmpId = "aero_virtio_tablet0"
if ($VerifyVirtioSndWav) {
  if (-not $WithVirtioSnd) {
    throw "-VerifyVirtioSndWav requires -WithVirtioSnd."
  }
  if ($VirtioSndAudioBackend -ne "wav") {
    throw "-VerifyVirtioSndWav requires -VirtioSndAudioBackend wav."
  }
}

if ($RequireVirtioSndMsix -and (-not $WithVirtioSnd)) {
  throw "-RequireVirtioSndMsix requires -WithVirtioSnd."
}

if ($WithSndBufferLimits -and (-not $WithVirtioSnd)) {
  throw "-WithSndBufferLimits requires -WithVirtioSnd (the buffer limits stress test only runs when a virtio-snd device is attached)."
}

if ($VirtioTransitional -and $WithVirtioSnd) {
  throw "-VirtioTransitional is incompatible with -WithVirtioSnd (virtio-snd testing requires modern-only virtio-pci + contract revision overrides)."
}

if ($VirtioMsixVectors -lt 0) {
  throw "-VirtioMsixVectors must be a positive integer."
}
if ($VirtioNetVectors -lt 0) {
  throw "-VirtioNetVectors must be a positive integer."
}
if ($VirtioBlkVectors -lt 0) {
  throw "-VirtioBlkVectors must be a positive integer."
}
if ($VirtioSndVectors -lt 0) {
  throw "-VirtioSndVectors must be a positive integer."
}
if ($VirtioInputVectors -lt 0) {
  throw "-VirtioInputVectors must be a positive integer."
}

if ($RequireIntx -and $RequireMsi) {
  throw "-RequireIntx and -RequireMsi are mutually exclusive."
}

if ($VirtioSndVectors -gt 0 -and (-not $WithVirtioSnd)) {
  throw "-VirtioSndVectors requires -WithVirtioSnd."
}

function Resolve-AeroWin7QemuMsixVectors {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$QemuSystem,
    [Parameter(Mandatory = $true)]
    [string]$DeviceName,
    [Parameter(Mandatory = $true)]
    [int]$Vectors,
    [Parameter(Mandatory = $true)]
    [string]$ParamName
  )

  if ($Vectors -le 0) { return 0 }

  $helpText = $null
  try {
    $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $DeviceName
  } catch {
    Write-Warning "Failed to query QEMU device help for '$DeviceName' while applying $ParamName=$Vectors. Ignoring vectors override. $_"
    return 0
  }
  if ($helpText -notmatch "(?m)^\s*vectors\b") {
    Write-Warning "QEMU device '$DeviceName' does not advertise a 'vectors' property; ignoring $ParamName=$Vectors"
    return 0
  }
  return $Vectors
}

if (-not $DisableUdp) {
  if ($UdpPort -le 0 -or $UdpPort -gt 65535) {
    throw "-UdpPort must be in the range 1..65535."
  }
}

$virtioSndPciDeviceName = ""
if ($WithVirtioSnd) {
  # Fail fast with a clear message if the selected QEMU binary lacks virtio-snd support (or the
  # device properties needed for the Aero contract v1 identity).
  $virtioSndPciDeviceName = Assert-AeroWin7QemuSupportsVirtioSndPciDevice -QemuSystem $QemuSystem
}

function Start-AeroSelftestHttpServer {
  param(
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $true)] [string]$Path
  )

  $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
  $listener.Start()
  return $listener
}

function Start-AeroSelftestUdpEchoServer {
  param(
    [Parameter(Mandatory = $true)] [int]$Port
  )

  $sock = [System.Net.Sockets.Socket]::new(
    [System.Net.Sockets.AddressFamily]::InterNetwork,
    [System.Net.Sockets.SocketType]::Dgram,
    [System.Net.Sockets.ProtocolType]::Udp
  )
  try {
    $sock.Bind([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, $Port))
    return $sock
  } catch {
    try { $sock.Close() } catch { }
    throw
  }
}

function Try-HandleAeroUdpEchoRequest {
  param(
    [Parameter(Mandatory = $true)] $Socket,
    [Parameter(Mandatory = $false)] [int]$MaxDatagramSize = 2048
  )

  if ($null -eq $Socket) { return $false }

  # Handle a small bounded number of datagrams per call so we never starve the main wait loop.
  $handledAny = $false
  $buf = New-Object byte[] ([Math]::Max(1, $MaxDatagramSize + 1))
  $remote = [System.Net.EndPoint]([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0))

  for ($i = 0; $i -lt 16; $i++) {
    try {
      # Non-blocking poll (0 microseconds).
      if (-not $Socket.Poll(0, [System.Net.Sockets.SelectMode]::SelectRead)) { break }
      $n = $Socket.ReceiveFrom($buf, 0, $buf.Length, [System.Net.Sockets.SocketFlags]::None, [ref]$remote)
      if ($n -le 0) { break }
      $handledAny = $true

      # Drop oversize datagrams (bounded/deterministic).
      if ($n -gt $MaxDatagramSize) { continue }

      $null = $Socket.SendTo($buf, 0, $n, [System.Net.Sockets.SocketFlags]::None, $remote)
    } catch {
      # Best-effort: never fail the harness due to UDP socket errors.
      break
    }
  }

  return $handledAny
}

$script:AeroSelftestLargePayload = $null
function Get-AeroSelftestLargePayload {
  # Lazily construct a deterministic 1 MiB payload (0..255 repeating).
  #
  # Avoid doing 1M PowerShell loop iterations for every request; this keeps the
  # harness responsive while the guest is downloading the body.
  if ($null -ne $script:AeroSelftestLargePayload) { return $script:AeroSelftestLargePayload }

  $size = 1048576
  $payload = New-Object byte[] $size

  $pattern = New-Object byte[] 256
  for ($i = 0; $i -lt 256; $i++) {
    $pattern[$i] = [byte]$i
  }

  for ($offset = 0; $offset -lt $size; $offset += 256) {
    [System.Buffer]::BlockCopy($pattern, 0, $payload, $offset, 256)
  }

  $script:AeroSelftestLargePayload = $payload
  return $payload
}

function Get-AeroSelftestLargePath {
  param(
    [Parameter(Mandatory = $true)] [string]$Path
  )

  # Compute "<path>-large" but insert before any query/fragment delimiter so a
  # configured HttpPath like "/foo?x=y" becomes "/foo-large?x=y".
  #
  # For backwards compatibility, the request handler also accepts "$Path-large"
  # verbatim (even when it would technically place "-large" after the query).
  $q = $Path.IndexOf("?")
  $h = $Path.IndexOf("#")
  $insertPos = -1
  if ($q -ge 0 -and $h -ge 0) {
    $insertPos = [Math]::Min($q, $h)
  } elseif ($q -ge 0) {
    $insertPos = $q
  } elseif ($h -ge 0) {
    $insertPos = $h
  }

  if ($insertPos -lt 0) { return "$Path-large" }
  return $Path.Insert($insertPos, "-large")
}

function Try-HandleAeroHttpRequest {
  param(
    [Parameter(Mandatory = $true)] $Listener,
    [Parameter(Mandatory = $true)] [string]$Path
  )

  if (-not $Listener.Pending()) { return $false }

  $client = $Listener.AcceptTcpClient()
  # Defensive timeouts: if the guest connects but stalls (or stops reading mid-body),
  # don't block the harness wait loop indefinitely.
  $client.ReceiveTimeout = 60000
  $client.SendTimeout = 60000
  try {
    try {
      $stream = $client.GetStream()
      $stream.ReadTimeout = 60000
      $stream.WriteTimeout = 60000
      # Use Latin-1 so we can safely read an arbitrary binary upload body via StreamReader
      # without losing bytes > 0x7F. Headers are ASCII-compatible.
      $enc = [System.Text.Encoding]::GetEncoding(28591)
      $reader = [System.IO.StreamReader]::new($stream, $enc, $false, 4096, $true)
      $requestLine = $reader.ReadLine()
      if ($null -eq $requestLine) { return $true }

      # Read+parse headers.
      $headers = @{}
      while ($true) {
        $line = $reader.ReadLine()
        if ($null -eq $line -or $line.Length -eq 0) { break }
        $idx = $line.IndexOf(":")
        if ($idx -gt 0) {
          $name = $line.Substring(0, $idx).Trim().ToLowerInvariant()
          $value = $line.Substring($idx + 1).Trim()
          if (-not [string]::IsNullOrEmpty($name)) {
            $headers[$name] = $value
          }
        }
      }

      $method = ""
      $reqPath = ""
      if ($requestLine -match "^(GET|HEAD|POST)\s+(\S+)\s+HTTP/") {
        $method = $Matches[1].ToUpperInvariant()
        $reqPath = $Matches[2]
      }

      $isHead = $method -eq "HEAD"
      $isPost = $method -eq "POST"

      $basePathOk = $reqPath -eq $Path
      $largePathOk = $reqPath -eq "$Path-large" -or $reqPath -eq (Get-AeroSelftestLargePath -Path $Path)

      $statusCode = 404
      $statusLine = "HTTP/1.1 404 Not Found"
      $contentType = "text/plain"
      $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("NOT_FOUND`n")
      $etagHeader = $null
      $extraHeaders = @()

      if (-not [string]::IsNullOrEmpty($method)) {
        if ($isPost) {
          # POST is only supported for the large endpoint (upload verification).
          if ($largePathOk) {
            $expectedLen = 1048576
            $cl = 0
            if ($headers.ContainsKey("content-length")) {
              [int]::TryParse($headers["content-length"], [ref]$cl) | Out-Null
            }

            $uploadOk = $false
            $uploadSha256 = ""
            if ($cl -eq $expectedLen) {
              if ($headers.ContainsKey("expect")) {
                $expectValue = $headers["expect"]
                if (-not [string]::IsNullOrEmpty($expectValue) -and $expectValue.ToLowerInvariant().Contains("100-continue")) {
                  # Some HTTP clients (including WinHTTP) may send `Expect: 100-continue` for large uploads.
                  # Reply with an interim 100 so the client proceeds to send the request body.
                  $continueBytes = [System.Text.Encoding]::ASCII.GetBytes("HTTP/1.1 100 Continue`r`n`r`n")
                  $stream.Write($continueBytes, 0, $continueBytes.Length)
                  $stream.Flush()
                }
              }

              $sha = [System.Security.Cryptography.SHA256]::Create()
              $emptyBytes = New-Object byte[] 0
              $charBuf = New-Object char[] 8192
              $byteBuf = New-Object byte[] 8192
              $remaining = $expectedLen
              while ($remaining -gt 0) {
                $want = [Math]::Min($charBuf.Length, $remaining)
                $readChars = $reader.Read($charBuf, 0, $want)
                if ($readChars -le 0) { break }
                $nBytes = $enc.GetBytes($charBuf, 0, $readChars, $byteBuf, 0)
                $null = $sha.TransformBlock($byteBuf, 0, $nBytes, $null, 0)
                $remaining -= $nBytes
              }
              if ($remaining -eq 0) {
                $null = $sha.TransformFinalBlock($emptyBytes, 0, 0)
                $uploadSha256 = ([System.BitConverter]::ToString($sha.Hash).Replace("-", "").ToLowerInvariant())
                if ($uploadSha256 -eq "fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83") {
                  $uploadOk = $true
                }
              }
              $sha.Dispose()
            }

            if ($uploadSha256.Length -gt 0) {
              $extraHeaders += "X-Aero-Upload-SHA256: $uploadSha256"
            }

            if ($uploadOk) {
              $statusCode = 200
              $statusLine = "HTTP/1.1 200 OK"
              $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("OK`n")
            } else {
              $statusCode = 400
              $statusLine = "HTTP/1.1 400 Bad Request"
              $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("BAD_UPLOAD`n")
            }
          }
        } else {
          # GET/HEAD.
          if ($basePathOk) {
            $statusCode = 200
            $statusLine = "HTTP/1.1 200 OK"
            $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes("OK`n")
          } elseif ($largePathOk) {
            $statusCode = 200
            $statusLine = "HTTP/1.1 200 OK"
            # Deterministic 1 MiB payload (0..255 repeating) for sustained virtio-net TX/RX stress.
            $contentType = "application/octet-stream"
            $etagHeader = "ETag: `"8505ae4435522325`""
            $bodyBytes = Get-AeroSelftestLargePayload
          }
        }
      }

      $hdrLines = @(
        $statusLine,
        "Content-Type: $contentType",
        "Content-Length: $($bodyBytes.Length)",
        "Cache-Control: no-store",
        $etagHeader,
        $extraHeaders,
        "Connection: close",
        "",
        ""
      ) | Where-Object { $null -ne $_ -and $_.Length -gt 0 }
      $hdr = ($hdrLines + @("", "")) -join "`r`n"

      $hdrBytes = [System.Text.Encoding]::ASCII.GetBytes($hdr)
      $stream.Write($hdrBytes, 0, $hdrBytes.Length)
      if (-not $isHead) {
        # Write in chunks so a large body (1 MiB) can't block forever behind a single large write
        # if the guest stalls mid-transfer. Socket/stream timeouts are still enforced.
        $chunkSize = 65536
        for ($offset = 0; $offset -lt $bodyBytes.Length; $offset += $chunkSize) {
          $count = [Math]::Min($chunkSize, $bodyBytes.Length - $offset)
          $stream.Write($bodyBytes, $offset, $count)
        }
      }
      $stream.Flush()
      return $true
    } catch {
      # Best-effort: never fail the harness due to an HTTP socket error; the guest selftest
      # will report connectivity failures via serial markers.
      return $true
    }
  } finally {
    $client.Close()
  }
}

function Read-NewText {
  param(
    [Parameter(Mandatory = $true)] [string]$Path,
    [Parameter(Mandatory = $true)] [ref]$Position
  )

  if (-not (Test-Path -LiteralPath $Path)) { return "" }

  $fs = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
  try {
    $null = $fs.Seek($Position.Value, [System.IO.SeekOrigin]::Begin)
    $buf = New-Object byte[] 8192
    $n = $fs.Read($buf, 0, $buf.Length)
    if ($n -le 0) { return "" }
    $Position.Value += $n

    return [System.Text.Encoding]::UTF8.GetString($buf, 0, $n)
  } finally {
    $fs.Dispose()
  }
}

function Wait-AeroSelftestResult {
  param(
    [Parameter(Mandatory = $true)] [string]$SerialLogPath,
    [Parameter(Mandatory = $true)] [System.Diagnostics.Process]$QemuProcess,
    [Parameter(Mandatory = $true)] [int]$TimeoutSeconds,
    [Parameter(Mandatory = $true)] $HttpListener,
    [Parameter(Mandatory = $true)] [string]$HttpPath,
    [Parameter(Mandatory = $false)] $UdpSocket = $null,
    [Parameter(Mandatory = $true)] [bool]$FollowSerial,
    # When $true, require per-test markers so older selftest binaries cannot accidentally pass.
    [Parameter(Mandatory = $false)] [bool]$RequirePerTestMarkers = $true,
    # When true, require the guest virtio-net-udp marker to PASS.
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioNetUdpPass = $true,
    # If true, a virtio-snd device was attached, so the virtio-snd selftest must actually run and pass
    # (not be skipped via --disable-snd).
    [Parameter(Mandatory = $true)] [bool]$RequireVirtioSndPass,
    # If true, require the optional virtio-snd-buffer-limits stress test marker to PASS.
    # This is intended to be paired with provisioning the guest with `--test-snd-buffer-limits`.
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioSndBufferLimitsPass = $false,
    # If true, require the optional virtio-input-events marker to PASS (host will inject events via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputEvents")]
    [bool]$RequireVirtioInputEventsPass = $false,
    # If true, require the optional virtio-input-media-keys marker to PASS (host will inject Consumer Control
    # (media key) events via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputMediaKeys")]
    [bool]$RequireVirtioInputMediaKeysPass = $false,
    # If true, require the optional virtio-input-tablet-events marker to PASS (host will inject absolute-pointer events
    # via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputTabletEvents")]
    [bool]$RequireVirtioInputTabletEventsPass = $false,
    # If true, also require the optional virtio-input-wheel marker to PASS (host will inject wheel/hscroll via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputWheel")]
    [bool]$RequireVirtioInputWheelPass = $false,
    # If true, require additional virtio-input-events-* markers (modifiers/buttons/wheel) to PASS.
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputEventsExtended")]
    [bool]$RequireVirtioInputEventsExtendedPass = $false,

    # If true, require the virtio-input-msix marker to report mode=msix.
    [Parameter(Mandatory = $false)]
    [bool]$RequireVirtioInputMsixPass = $false,
    # If true, require the guest selftest to be configured to fail virtio-blk on INTx (expect MSI/MSI-X),
    # i.e. `AERO_VIRTIO_SELFTEST|CONFIG|...|expect_blk_msi=1`.
    [Parameter(Mandatory = $false)]
    [bool]$RequireExpectBlkMsi = $false,
    # Best-effort QMP channel for input injection.
    [Parameter(Mandatory = $false)] [string]$QmpHost = "127.0.0.1",
    [Parameter(Mandatory = $false)] [Nullable[int]]$QmpPort = $null
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $pos = 0L
  $tail = ""
  $configExpectBlkMsi = $null
  $sawConfigExpectBlkMsi = $false
  $sawVirtioBlkPass = $false
  $sawVirtioBlkFail = $false
  $sawVirtioInputPass = $false
  $sawVirtioInputFail = $false
  $virtioInputMarkerTime = $null
  $sawVirtioInputEventsReady = $false
  $sawVirtioInputEventsPass = $false
  $sawVirtioInputEventsFail = $false
  $sawVirtioInputEventsSkip = $false
  $sawVirtioInputWheelPass = $false
  $sawVirtioInputWheelFail = $false
  $sawVirtioInputWheelSkip = $false
  $sawVirtioInputEventsModifiersPass = $false
  $sawVirtioInputEventsModifiersFail = $false
  $sawVirtioInputEventsModifiersSkip = $false
  $sawVirtioInputEventsButtonsPass = $false
  $sawVirtioInputEventsButtonsFail = $false
  $sawVirtioInputEventsButtonsSkip = $false
  $sawVirtioInputEventsWheelPass = $false
  $sawVirtioInputEventsWheelFail = $false
  $sawVirtioInputEventsWheelSkip = $false
  $inputEventsInjectAttempts = 0
  $maxInputEventsInjectAttempts = if ($RequireVirtioInputEventsExtendedPass) { 30 } else { 20 }
  $nextInputEventsInject = [DateTime]::UtcNow
  $sawVirtioInputMediaKeysReady = $false
  $sawVirtioInputMediaKeysPass = $false
  $sawVirtioInputMediaKeysFail = $false
  $sawVirtioInputMediaKeysSkip = $false
  $inputMediaKeysInjectAttempts = 0
  $nextInputMediaKeysInject = [DateTime]::UtcNow
  $sawVirtioInputTabletEventsReady = $false
  $sawVirtioInputTabletEventsPass = $false
  $sawVirtioInputTabletEventsFail = $false
  $sawVirtioInputTabletEventsSkip = $false
  $inputTabletEventsInjectAttempts = 0
  $nextInputTabletEventsInject = [DateTime]::UtcNow
  $sawVirtioSndPass = $false
  $sawVirtioSndSkip = $false
  $sawVirtioSndFail = $false
  $sawVirtioSndCapturePass = $false
  $sawVirtioSndCaptureSkip = $false
  $sawVirtioSndCaptureFail = $false
  $sawVirtioSndDuplexPass = $false
  $sawVirtioSndDuplexSkip = $false
  $sawVirtioSndDuplexFail = $false
  $sawVirtioSndBufferLimitsPass = $false
  $sawVirtioSndBufferLimitsSkip = $false
  $sawVirtioSndBufferLimitsFail = $false
  $sawVirtioNetPass = $false
  $sawVirtioNetFail = $false
  $sawVirtioNetUdpPass = $false
  $sawVirtioNetUdpFail = $false
  $sawVirtioNetUdpSkip = $false

  function Test-VirtioInputMsixRequirement {
    param(
      [Parameter(Mandatory = $true)] [string]$Tail
    )

    if (-not $RequireVirtioInputMsixPass) { return $null }

    $matches = [regex]::Matches($Tail, "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-msix\|[^\r\n]+")
    if ($matches.Count -le 0) {
      return @{ Result = "MISSING_VIRTIO_INPUT_MSIX"; Tail = $Tail }
    }

    $line = $matches[$matches.Count - 1].Value
    $status = ""
    if ($line -match "^AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-msix\|([^|]+)") {
      $status = $Matches[1]
    }

    $mode = ""
    if ($line -match "mode=([^|\r\n]+)") { $mode = $Matches[1] }

    if ($status -ne "PASS" -or $mode -ne "msix") {
      return @{ Result = "VIRTIO_INPUT_MSIX_REQUIRED"; Tail = $Tail }
    }

    return $null
  }

  while ((Get-Date) -lt $deadline) {
    $null = Try-HandleAeroHttpRequest -Listener $HttpListener -Path $HttpPath
    if ($null -ne $UdpSocket) {
      $null = Try-HandleAeroUdpEchoRequest -Socket $UdpSocket
    }

    $chunk = Read-NewText -Path $SerialLogPath -Position ([ref]$pos)
    if ($chunk.Length -gt 0) {
      if ($FollowSerial) {
        [Console]::Out.Write($chunk)
      }

      $tail += $chunk
      if ($tail.Length -gt 131072) { $tail = $tail.Substring($tail.Length - 131072) }

      if ($RequireExpectBlkMsi -and (-not $sawConfigExpectBlkMsi)) {
        # Parse the guest selftest CONFIG marker to ensure the image was provisioned
        # with `--expect-blk-msi` (expect_blk_msi=1). This provides deterministic
        # harness-side gating for MSI/MSI-X-specific CI.
        $prefix = "AERO_VIRTIO_SELFTEST|CONFIG|"
        $matches = [regex]::Matches($tail, [regex]::Escape($prefix) + "[^`r`n]*")
        if ($matches.Count -gt 0) {
          $line = $matches[$matches.Count - 1].Value
          if ($line -match "(?:^|\\|)expect_blk_msi=(0|1)(?:\\||$)") {
            $configExpectBlkMsi = $Matches[1]
            $sawConfigExpectBlkMsi = $true
            if ($configExpectBlkMsi -ne "1") {
              return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
            }
          }
        }
      }

      if (-not $sawVirtioBlkPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|PASS") {
        $sawVirtioBlkPass = $true
      }
      if (-not $sawVirtioBlkFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|FAIL") {
        $sawVirtioBlkFail = $true
      }
      if (-not $sawVirtioInputPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|PASS") {
        $sawVirtioInputPass = $true
        if ($null -eq $virtioInputMarkerTime) { $virtioInputMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioInputFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|FAIL") {
        $sawVirtioInputFail = $true
        if ($null -eq $virtioInputMarkerTime) { $virtioInputMarkerTime = [DateTime]::UtcNow }
      }
      if (-not $sawVirtioInputEventsReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|READY") {
        $sawVirtioInputEventsReady = $true
      }
      if (-not $sawVirtioInputEventsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|PASS") {
        $sawVirtioInputEventsPass = $true
      }
      if (-not $sawVirtioInputEventsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|FAIL") {
        $sawVirtioInputEventsFail = $true
      }
      if (-not $sawVirtioInputEventsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events\|SKIP") {
        $sawVirtioInputEventsSkip = $true
      }
      if (-not $sawVirtioInputMediaKeysReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|READY") {
        $sawVirtioInputMediaKeysReady = $true
      }
      if (-not $sawVirtioInputMediaKeysPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|PASS") {
        $sawVirtioInputMediaKeysPass = $true
      }
      if (-not $sawVirtioInputMediaKeysFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|FAIL") {
        $sawVirtioInputMediaKeysFail = $true
      }
      if (-not $sawVirtioInputMediaKeysSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-media-keys\|SKIP") {
        $sawVirtioInputMediaKeysSkip = $true
      }
      if (-not $sawVirtioInputWheelPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|PASS") {
        $sawVirtioInputWheelPass = $true
      }
      if (-not $sawVirtioInputWheelFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|FAIL") {
        $sawVirtioInputWheelFail = $true
      }
      if (-not $sawVirtioInputWheelSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-wheel\|SKIP") {
        $sawVirtioInputWheelSkip = $true
      }
      if (-not $sawVirtioInputEventsModifiersPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|PASS") {
        $sawVirtioInputEventsModifiersPass = $true
      }
      if (-not $sawVirtioInputEventsModifiersFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|FAIL") {
        $sawVirtioInputEventsModifiersFail = $true
      }
      if (-not $sawVirtioInputEventsModifiersSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-modifiers\|SKIP") {
        $sawVirtioInputEventsModifiersSkip = $true
      }
      if (-not $sawVirtioInputEventsButtonsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|PASS") {
        $sawVirtioInputEventsButtonsPass = $true
      }
      if (-not $sawVirtioInputEventsButtonsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|FAIL") {
        $sawVirtioInputEventsButtonsFail = $true
      }
      if (-not $sawVirtioInputEventsButtonsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-buttons\|SKIP") {
        $sawVirtioInputEventsButtonsSkip = $true
      }
      if (-not $sawVirtioInputEventsWheelPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|PASS") {
        $sawVirtioInputEventsWheelPass = $true
      }
      if (-not $sawVirtioInputEventsWheelFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|FAIL") {
        $sawVirtioInputEventsWheelFail = $true
      }
      if (-not $sawVirtioInputEventsWheelSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-events-wheel\|SKIP") {
        $sawVirtioInputEventsWheelSkip = $true
      }

      # If input events are required, fail fast when the guest reports SKIP/FAIL for virtio-input-events.
      # This saves time when the guest image was provisioned without `--test-input-events`, or when the
      # end-to-end input report delivery path is broken.
      if ($RequireVirtioInputEventsPass) {
        if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
        if ($RequireVirtioInputEventsExtendedPass) {
          if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
            return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
          }
          if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
            return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
          }
        }
      }
      if ($RequireVirtioInputMediaKeysPass) {
        if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
      }
      if ($RequireVirtioInputWheelPass) {
        if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
      }

      if (-not $sawVirtioInputTabletEventsReady -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|READY") {
        $sawVirtioInputTabletEventsReady = $true
      }
      if (-not $sawVirtioInputTabletEventsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|PASS") {
        $sawVirtioInputTabletEventsPass = $true
      }
      if (-not $sawVirtioInputTabletEventsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|FAIL") {
        $sawVirtioInputTabletEventsFail = $true
      }
      if (-not $sawVirtioInputTabletEventsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input-tablet-events\|SKIP") {
        $sawVirtioInputTabletEventsSkip = $true
      }

      # If tablet events are required, fail fast when the guest reports SKIP/FAIL for virtio-input-tablet-events.
      if ($RequireVirtioInputTabletEventsPass) {
        if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
      }
      if (-not $sawVirtioSndPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|PASS") {
        $sawVirtioSndPass = $true
      }
      if (-not $sawVirtioSndSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|SKIP") {
        $sawVirtioSndSkip = $true
      }
      if (-not $sawVirtioSndFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL") {
        $sawVirtioSndFail = $true
      }
      if (-not $sawVirtioSndCapturePass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|PASS") {
        $sawVirtioSndCapturePass = $true
      }
      if (-not $sawVirtioSndCaptureSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|SKIP") {
        $sawVirtioSndCaptureSkip = $true
      }
      if (-not $sawVirtioSndCaptureFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-capture\|FAIL") {
        $sawVirtioSndCaptureFail = $true
      }
      if (-not $sawVirtioSndDuplexPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|PASS") {
        $sawVirtioSndDuplexPass = $true
      }
      if (-not $sawVirtioSndDuplexSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|SKIP") {
        $sawVirtioSndDuplexSkip = $true
      }
      if (-not $sawVirtioSndDuplexFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-duplex\|FAIL") {
        $sawVirtioSndDuplexFail = $true
      }
      if (-not $sawVirtioSndBufferLimitsPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|PASS") {
        $sawVirtioSndBufferLimitsPass = $true
      }
      if (-not $sawVirtioSndBufferLimitsSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|SKIP") {
        $sawVirtioSndBufferLimitsSkip = $true
      }
      if (-not $sawVirtioSndBufferLimitsFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd-buffer-limits\|FAIL") {
        $sawVirtioSndBufferLimitsFail = $true
      }
      if (-not $sawVirtioNetPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|PASS") {
        $sawVirtioNetPass = $true
      }
      if (-not $sawVirtioNetFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|FAIL") {
        $sawVirtioNetFail = $true
      }
      if (-not $sawVirtioNetUdpPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|PASS") {
        $sawVirtioNetUdpPass = $true
      }
      if (-not $sawVirtioNetUdpFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|FAIL") {
        $sawVirtioNetUdpFail = $true
      }
      if (-not $sawVirtioNetUdpSkip -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net-udp\|SKIP") {
        $sawVirtioNetUdpSkip = $true
      }

      if ($RequireVirtioSndBufferLimitsPass) {
        if ($sawVirtioSndBufferLimitsSkip) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail } }
        if ($sawVirtioSndBufferLimitsFail) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail } }
      }

      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|PASS") {
        if ($RequireExpectBlkMsi -and ((-not $sawConfigExpectBlkMsi) -or $configExpectBlkMsi -ne "1")) {
          return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
        }
        if ($RequirePerTestMarkers) {
          # Require per-test markers so older selftest binaries cannot accidentally pass the host harness.
          if ($sawVirtioBlkFail) {
            return @{ Result = "VIRTIO_BLK_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioBlkPass) {
            return @{ Result = "MISSING_VIRTIO_BLK"; Tail = $tail }
          }
          if ($sawVirtioInputFail) {
            return @{ Result = "VIRTIO_INPUT_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioInputPass) {
            return @{ Result = "MISSING_VIRTIO_INPUT"; Tail = $tail }
          }

          # Also ensure the virtio-snd markers are present (playback + capture), so older selftest binaries
          # that predate virtio-snd testing cannot accidentally pass.
          if ($sawVirtioSndFail) {
            return @{ Result = "VIRTIO_SND_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndPass -or $sawVirtioSndSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndPass)) {
            return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndCaptureFail) {
            return @{ Result = "VIRTIO_SND_CAPTURE_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndCapturePass -or $sawVirtioSndCaptureSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_CAPTURE"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndCapturePass)) {
            return @{ Result = "VIRTIO_SND_CAPTURE_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndDuplexFail) {
            return @{ Result = "VIRTIO_SND_DUPLEX_FAILED"; Tail = $tail }
          }
          if (-not ($sawVirtioSndDuplexPass -or $sawVirtioSndDuplexSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_DUPLEX"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndDuplexPass)) {
            return @{ Result = "VIRTIO_SND_DUPLEX_SKIPPED"; Tail = $tail }
          }

          if ($RequireVirtioSndBufferLimitsPass) {
            if ($sawVirtioSndBufferLimitsFail) {
              return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioSndBufferLimitsPass) {
              if ($sawVirtioSndBufferLimitsSkip) {
                return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_SND_BUFFER_LIMITS"; Tail = $tail }
            }
          }

          if ($sawVirtioNetFail) {
            return @{ Result = "VIRTIO_NET_FAILED"; Tail = $tail }
          }
          if (-not $sawVirtioNetPass) {
            return @{ Result = "MISSING_VIRTIO_NET"; Tail = $tail }
          }
          if ($RequireVirtioNetUdpPass) {
            if ($sawVirtioNetUdpFail) {
              return @{ Result = "VIRTIO_NET_UDP_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioNetUdpPass) {
              if ($sawVirtioNetUdpSkip) { return @{ Result = "VIRTIO_NET_UDP_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_NET_UDP"; Tail = $tail }
            }
          }

          if ($RequireVirtioInputEventsPass) {
            if ($sawVirtioInputEventsFail) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioInputEventsPass) {
              if ($sawVirtioInputEventsSkip) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
            }
            if ($RequireVirtioInputEventsExtendedPass) {
              if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
              }
              if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
                return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
              }
              if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
                return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
              }
            }
          }
          if ($RequireVirtioInputMediaKeysPass) {
            if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
            if (-not $sawVirtioInputMediaKeysPass) {
              if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
            }
          }
          if ($RequireVirtioInputTabletEventsPass) {
            if ($sawVirtioInputTabletEventsFail) {
              return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail }
            }
            if (-not $sawVirtioInputTabletEventsPass) {
              if ($sawVirtioInputTabletEventsSkip) {
                return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
            }
          }
          if ($RequireVirtioInputWheelPass) {
            if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
            if (-not $sawVirtioInputWheelPass) {
              if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
              return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
            }
          }

          $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail
          if ($null -ne $msixCheck) { return $msixCheck }
          return @{ Result = "PASS"; Tail = $tail }
        }

        if ($RequireVirtioSndPass) {
          if ($sawVirtioSndFail) {
            return @{ Result = "VIRTIO_SND_FAILED"; Tail = $tail }
          }
          if ($sawVirtioSndPass) {
            if ($sawVirtioSndCaptureFail) { return @{ Result = "VIRTIO_SND_CAPTURE_FAILED"; Tail = $tail } }
            if ($sawVirtioSndDuplexFail) { return @{ Result = "VIRTIO_SND_DUPLEX_FAILED"; Tail = $tail } }
            if ($sawVirtioSndCapturePass) {
              if ($sawVirtioSndDuplexPass) {
                if ($RequireVirtioSndBufferLimitsPass) {
                  if ($sawVirtioSndBufferLimitsFail) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioSndBufferLimitsPass) {
                    if ($sawVirtioSndBufferLimitsSkip) { return @{ Result = "VIRTIO_SND_BUFFER_LIMITS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_SND_BUFFER_LIMITS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputEventsPass) {
                  if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputEventsPass) {
                    if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
                  }
                  if ($RequireVirtioInputEventsExtendedPass) {
                    if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
                      return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
                    }
                    if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
                      return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
                    }
                    if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
                      return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
                    }
                  }
                }
                if ($RequireVirtioInputMediaKeysPass) {
                  if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputMediaKeysPass) {
                    if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputTabletEventsPass) {
                  if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputTabletEventsPass) {
                    if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
                  }
                }
                if ($RequireVirtioInputWheelPass) {
                  if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputWheelPass) {
                    if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
                  }
                }
                $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail
                if ($null -ne $msixCheck) { return $msixCheck }
                return @{ Result = "PASS"; Tail = $tail }
              }
              if ($sawVirtioSndDuplexSkip) {
                return @{ Result = "VIRTIO_SND_DUPLEX_SKIPPED"; Tail = $tail }
              }
              return @{ Result = "MISSING_VIRTIO_SND_DUPLEX"; Tail = $tail }
            }
            if ($sawVirtioSndCaptureSkip) {
              return @{ Result = "VIRTIO_SND_CAPTURE_SKIPPED"; Tail = $tail }
            }
            return @{ Result = "MISSING_VIRTIO_SND_CAPTURE"; Tail = $tail }
          }
          if ($sawVirtioSndSkip) {
            return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
          }
          return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
        }

        if ($RequireVirtioInputEventsPass) {
          if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputEventsPass) {
            if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
          }
          if ($RequireVirtioInputEventsExtendedPass) {
            if ($sawVirtioInputEventsModifiersFail -or $sawVirtioInputEventsButtonsFail -or $sawVirtioInputEventsWheelFail) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"; Tail = $tail }
            }
            if ($sawVirtioInputEventsModifiersSkip -or $sawVirtioInputEventsButtonsSkip -or $sawVirtioInputEventsWheelSkip) {
              return @{ Result = "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"; Tail = $tail }
            }
            if (-not ($sawVirtioInputEventsModifiersPass -and $sawVirtioInputEventsButtonsPass -and $sawVirtioInputEventsWheelPass)) {
              return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED"; Tail = $tail }
            }
          }
        }
        if ($RequireVirtioInputMediaKeysPass) {
          if ($sawVirtioInputMediaKeysFail) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputMediaKeysPass) {
            if ($sawVirtioInputMediaKeysSkip) { return @{ Result = "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail }
          }
        }
        if ($RequireVirtioInputTabletEventsPass) {
          if ($sawVirtioInputTabletEventsFail) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputTabletEventsPass) {
            if ($sawVirtioInputTabletEventsSkip) { return @{ Result = "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail }
          }
        }
        if ($RequireVirtioInputWheelPass) {
          if ($sawVirtioInputWheelFail) { return @{ Result = "VIRTIO_INPUT_WHEEL_FAILED"; Tail = $tail } }
          if (-not $sawVirtioInputWheelPass) {
            if ($sawVirtioInputWheelSkip) { return @{ Result = "VIRTIO_INPUT_WHEEL_SKIPPED"; Tail = $tail } }
            return @{ Result = "MISSING_VIRTIO_INPUT_WHEEL"; Tail = $tail }
          }
        }

        $msixCheck = Test-VirtioInputMsixRequirement -Tail $tail
        if ($null -ne $msixCheck) { return $msixCheck }
        return @{ Result = "PASS"; Tail = $tail }
      }
      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|FAIL") {
        if ($RequireExpectBlkMsi -and ((-not $sawConfigExpectBlkMsi) -or $configExpectBlkMsi -ne "1")) {
          return @{ Result = "EXPECT_BLK_MSI_NOT_SET"; Tail = $tail }
        }
        return @{ Result = "FAIL"; Tail = $tail }
      }
    }

    # When requested, inject keyboard/mouse events after the guest has armed the user-mode HID report read loop
    # (virtio-input-events|READY). Inject multiple times on a short interval to reduce flakiness from timing
    # windows (reports may be dropped when no read is pending).
    #
    # If the guest never emits READY/SKIP/PASS/FAIL after completing virtio-input, assume the guest selftest
    # is too old (or misconfigured) and fail early to avoid burning the full virtio-net timeout.
    if ($RequireVirtioInputEventsPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputEventsReady) -and (-not $sawVirtioInputEventsPass) -and (-not $sawVirtioInputEventsFail) -and (-not $sawVirtioInputEventsSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail } }
    }
    if ($RequireVirtioInputMediaKeysPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputMediaKeysReady) -and (-not $sawVirtioInputMediaKeysPass) -and (-not $sawVirtioInputMediaKeysFail) -and (-not $sawVirtioInputMediaKeysSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_MEDIA_KEYS"; Tail = $tail } }
    }
    if ($RequireVirtioInputTabletEventsPass -and ($null -ne $virtioInputMarkerTime) -and (-not $sawVirtioInputTabletEventsReady) -and (-not $sawVirtioInputTabletEventsPass) -and (-not $sawVirtioInputTabletEventsFail) -and (-not $sawVirtioInputTabletEventsSkip)) {
      $delta = ([DateTime]::UtcNow - $virtioInputMarkerTime).TotalSeconds
      if ($delta -ge 20) { return @{ Result = "MISSING_VIRTIO_INPUT_TABLET_EVENTS"; Tail = $tail } }
    }
    if ($RequireVirtioInputEventsPass -and $sawVirtioInputEventsReady -and (-not $sawVirtioInputEventsPass) -and (-not $sawVirtioInputEventsFail) -and (-not $sawVirtioInputEventsSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
      }
      if ($inputEventsInjectAttempts -lt $maxInputEventsInjectAttempts -and [DateTime]::UtcNow -ge $nextInputEventsInject) {
        $inputEventsInjectAttempts++
        $nextInputEventsInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputEvents -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputEventsInjectAttempts -WithWheel:($RequireVirtioInputWheelPass -or $RequireVirtioInputEventsExtendedPass) -Extended:$RequireVirtioInputEventsExtendedPass
        if (-not $ok) {
          return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
        }
      }
    }

    if ($RequireVirtioInputMediaKeysPass -and $sawVirtioInputMediaKeysReady -and (-not $sawVirtioInputMediaKeysPass) -and (-not $sawVirtioInputMediaKeysFail) -and (-not $sawVirtioInputMediaKeysSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_MEDIA_KEYS_UNSUPPORTED"; Tail = $tail }
      }
      if ($inputMediaKeysInjectAttempts -lt 20 -and [DateTime]::UtcNow -ge $nextInputMediaKeysInject) {
        $inputMediaKeysInjectAttempts++
        $nextInputMediaKeysInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputMediaKeys -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputMediaKeysInjectAttempts
        if (-not $ok) {
          return @{ Result = "QMP_MEDIA_KEYS_UNSUPPORTED"; Tail = $tail }
        }
      }
    }

    if ($RequireVirtioInputTabletEventsPass -and $sawVirtioInputTabletEventsReady -and (-not $sawVirtioInputTabletEventsPass) -and (-not $sawVirtioInputTabletEventsFail) -and (-not $sawVirtioInputTabletEventsSkip)) {
      if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
        return @{ Result = "QMP_INPUT_TABLET_INJECT_FAILED"; Tail = $tail }
      }
      if ($inputTabletEventsInjectAttempts -lt 20 -and [DateTime]::UtcNow -ge $nextInputTabletEventsInject) {
        $inputTabletEventsInjectAttempts++
        $nextInputTabletEventsInject = [DateTime]::UtcNow.AddMilliseconds(500)
        $ok = Try-AeroQmpInjectVirtioInputTabletEvents -Host $QmpHost -Port ([int]$QmpPort) -Attempt $inputTabletEventsInjectAttempts
        if (-not $ok) {
          return @{ Result = "QMP_INPUT_TABLET_INJECT_FAILED"; Tail = $tail }
        }
      }
    }

    if ($QemuProcess.HasExited) {
      return @{
        Result = "QEMU_EXITED"
        Tail = $tail
      }
    }

    Start-Sleep -Milliseconds 250
  }

  return @{
    Result = "TIMEOUT"
    Tail = $tail
  }
}

function Get-AeroVirtioSoundDeviceArg {
  param(
    [Parameter(Mandatory = $true)] [string]$QemuSystem,
    # If true, require modern-only virtio-pci enumeration (disable-legacy=on) and contract revision (x-pci-revision=0x01).
    [Parameter(Mandatory = $true)] [bool]$ModernOnly,

    # Optional MSI-X vector count (`vectors=` device property).
    [Parameter(Mandatory = $false)] [int]$MsixVectors = 0,
    # Optional name of the PowerShell parameter that requested this override (for clearer warnings).
    [Parameter(Mandatory = $false)] [string]$VectorsParamName = "-VirtioMsixVectors",

    # Optional pre-resolved virtio-snd PCI device name (virtio-sound-pci / virtio-snd-pci).
    [Parameter(Mandatory = $false)] [string]$DeviceName = ""
  )

  $deviceName = $DeviceName
  if ([string]::IsNullOrEmpty($deviceName)) {
    # Determine which QEMU virtio-snd PCI device name is available and validate it supports
    # the Aero contract v1 configuration we need.
    #
    # The strict Aero INF (`aero_virtio_snd.inf`) matches only the modern virtio-snd PCI ID
    # (`PCI\VEN_1AF4&DEV_1059`) and requires `REV_01`, so we must:
    #   - force modern-only virtio-pci enumeration (`disable-legacy=on` => `DEV_1059`)
    #   - force PCI Revision ID 0x01 (`x-pci-revision=0x01` => `REV_01`)
    if ($ModernOnly) {
      $deviceName = Assert-AeroWin7QemuSupportsVirtioSndPciDevice -QemuSystem $QemuSystem
    } else {
      $deviceName = Resolve-AeroVirtioSndPciDeviceName -QemuSystem $QemuSystem
    }
  }

  $helpText = $null
  if ($ModernOnly -or $MsixVectors -gt 0) {
    $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $deviceName
  }

  if ($ModernOnly) {
    if ($helpText -notmatch "(?m)^\s*disable-legacy\b") {
      throw "QEMU device '$deviceName' does not expose 'disable-legacy'. AERO-W7-VIRTIO v1 virtio-snd requires modern-only virtio-pci enumeration (DEV_1059). Upgrade QEMU."
    }
    if ($helpText -notmatch "(?m)^\s*x-pci-revision\b") {
      throw "QEMU device '$deviceName' does not expose 'x-pci-revision'. AERO-W7-VIRTIO v1 virtio-snd requires PCI Revision ID 0x01 (REV_01). Upgrade QEMU."
    }
  }

  if ($MsixVectors -gt 0 -and ($helpText -notmatch "(?m)^\s*vectors\b")) {
    Write-Warning "QEMU device '$deviceName' does not advertise a 'vectors' property; ignoring $VectorsParamName=$MsixVectors"
    $MsixVectors = 0
  }

  $arg = "$deviceName"
  if ($ModernOnly) {
    $arg += ",disable-legacy=on,x-pci-revision=0x01"
  }
  $arg += ",audiodev=snd0"
  if ($MsixVectors -gt 0) {
    $arg += ",vectors=$MsixVectors"
  }
  return $arg
}

function Sanitize-AeroMarkerValue {
  param(
    [Parameter(Mandatory = $true)] [string]$Value
  )
  return $Value.Replace("|", "/").Replace("`r", " ").Replace("`n", " ").Trim()
}

function Try-ExtractLastAeroMarkerLine {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string]$Prefix,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $matches = [regex]::Matches($Tail, [regex]::Escape($Prefix) + "[^`r`n]*")
  if ($matches.Count -gt 0) {
    return $matches[$matches.Count - 1].Value
  }

  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($Prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      return $last
    }
  }

  return $null
}

function Try-EmitAeroVirtioBlkIrqMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain any virtio-blk IRQ marker data (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  # Collect IRQ fields from (a) the virtio-blk per-test marker (IOCTL-derived fields) and/or
  # (b) standalone diagnostics:
  #   - `virtio-blk-miniport-irq|...` (best-effort miniport IOCTL diagnostics; may include message_count + MSI-X vectors)
  #   - `virtio-blk-irq|...` (cfgmgr32 resource enumeration / Windows-assigned IRQ mode)
  #
  # Prefer the per-test marker when present, but fill in missing fields from the standalone
  # diagnostics so the host marker is still produced for older selftest binaries.
  $blkPrefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
  $blkMatches = [regex]::Matches($Tail, [regex]::Escape($blkPrefix) + "[^`r`n]*")
  $blkLine = $null
  if ($blkMatches.Count -gt 0) {
    $blkLine = $blkMatches[$blkMatches.Count - 1].Value
  }

  $fields = @{}
  $addLineFields = {
    param([string]$Line)
    if ([string]::IsNullOrEmpty($Line)) { return }
    foreach ($tok in $Line.Split("|")) {
      $idx = $tok.IndexOf("=")
      if ($idx -le 0) { continue }
      $k = $tok.Substring(0, $idx)
      $v = $tok.Substring($idx + 1)
      if (-not [string]::IsNullOrEmpty($k) -and (-not $fields.ContainsKey($k))) {
        $fields[$k] = $v
      }
    }
  }

  # Always merge fields from the per-test marker (may include irq_mode/msix_* fields).
  & $addLineFields $blkLine

  # Best-effort: accept legacy/selftest-internal virtio-blk IRQ marker variants (if any).
  foreach ($prefix in @(
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-irq|",
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|IRQ|",
      "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|INFO|",
      "AERO_VIRTIO_SELFTEST|MARKER|virtio-blk-irq|"
    )) {
    $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
    if ($matches.Count -gt 0) {
      & $addLineFields $matches[$matches.Count - 1].Value
      break
    }
  }

  # Standalone diagnostics markers emitted by the guest:
  # - miniport IOCTL diagnostics (best-effort; depends on miniport contract):
  #     virtio-blk-miniport-irq|INFO|mode=msi|messages=...|message_count=...|msix_config_vector=...|msix_queue0_vector=...
  # - resource enumeration (PnP):
  #     virtio-blk-irq|INFO|mode=msi|messages=...
  #
  # Prefer miniport diagnostics when present (newer guests reserve `virtio-blk-irq|...` for
  # cfgmgr32/Windows-assigned IRQ resources).
  $miniportIrqMatches = [regex]::Matches($Tail, "(?m)^\s*virtio-blk-miniport-irq\|[^`r`n]*")
  if ($miniportIrqMatches.Count -gt 0) {
    & $addLineFields $miniportIrqMatches[$miniportIrqMatches.Count - 1].Value
  }
  $irqMatches = [regex]::Matches($Tail, "(?m)^\s*virtio-blk-irq\|[^`r`n]*")
  if ($irqMatches.Count -gt 0) {
    & $addLineFields $irqMatches[$irqMatches.Count - 1].Value
  }

  $sawBlkLineInTail = ($blkMatches.Count -gt 0)
  $sawBlkIrqLineInTail = ($irqMatches.Count -gt 0 -or $miniportIrqMatches.Count -gt 0)

  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath) -and (-not ($sawBlkLineInTail -and $sawBlkIrqLineInTail))) {
    # Tail truncation fallback: scan the full serial log line-by-line and keep the last blk markers we care about.
    $lastBlkLine = $null
    $lastBlkIrqLine = $null
    $lastBlkMiniportIrqLine = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $line = $sr.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match "^\s*AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|") {
              $lastBlkLine = $line.Trim()
            }
            if ($line -match "^\s*virtio-blk-miniport-irq\|") {
              $lastBlkMiniportIrqLine = $line.Trim()
            }
            if ($line -match "^\s*virtio-blk-irq\|") {
              $lastBlkIrqLine = $line.Trim()
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }

    if ($null -eq $blkLine -and $null -ne $lastBlkLine) {
      $blkLine = $lastBlkLine
      & $addLineFields $lastBlkLine
    }
    if ($null -ne $lastBlkMiniportIrqLine) {
      & $addLineFields $lastBlkMiniportIrqLine
    }
    if ($null -ne $lastBlkIrqLine) {
      & $addLineFields $lastBlkIrqLine
    }
  }

  if ($fields.Count -eq 0) { return }

  $mode = $null
  # Prefer `irq_mode` from the per-test marker (IOCTL-derived) over `mode` from the standalone diagnostics,
  # since the IOCTL-based path can distinguish MSI vs MSI-X based on vector assignment.
  if ($fields.ContainsKey("irq_mode")) { $mode = $fields["irq_mode"] }
  elseif ($fields.ContainsKey("mode")) { $mode = $fields["mode"] }
  elseif ($fields.ContainsKey("interrupt_mode")) { $mode = $fields["interrupt_mode"] }

  $messages = $null
  # Prefer canonical `irq_message_count` when present.
  if ($fields.ContainsKey("irq_message_count")) { $messages = $fields["irq_message_count"] }
  elseif ($fields.ContainsKey("message_count")) { $messages = $fields["message_count"] }
  elseif ($fields.ContainsKey("messages")) { $messages = $fields["messages"] }
  elseif ($fields.ContainsKey("irq_messages")) { $messages = $fields["irq_messages"] }
  elseif ($fields.ContainsKey("msi_messages")) { $messages = $fields["msi_messages"] }

  $vectors = $null
  if ($fields.ContainsKey("irq_vectors")) { $vectors = $fields["irq_vectors"] }
  elseif ($fields.ContainsKey("vectors")) { $vectors = $fields["vectors"] }

  $msiVector = $null
  if ($fields.ContainsKey("msi_vector")) { $msiVector = $fields["msi_vector"] }
  elseif ($fields.ContainsKey("vector")) { $msiVector = $fields["vector"] }
  elseif ($fields.ContainsKey("irq_vector")) { $msiVector = $fields["irq_vector"] }

  $msixConfigVector = $null
  if ($fields.ContainsKey("msix_config_vector")) { $msixConfigVector = $fields["msix_config_vector"] }

  $msixQueueVector = $null
  if ($fields.ContainsKey("msix_queue_vector")) { $msixQueueVector = $fields["msix_queue_vector"] }
  elseif ($fields.ContainsKey("msix_queue0_vector")) { $msixQueueVector = $fields["msix_queue0_vector"] }

  if (-not $mode -and -not $messages -and -not $vectors -and -not $msiVector -and -not $msixConfigVector -and -not $msixQueueVector) { return }

  $status = "INFO"
  if (-not [string]::IsNullOrEmpty($blkLine)) {
    if ($blkLine -match "\|FAIL(\||$)") { $status = "FAIL" }
    elseif ($blkLine -match "\|PASS(\||$)") { $status = "PASS" }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|$status"
  if ($mode) { $out += "|irq_mode=$(Sanitize-AeroMarkerValue $mode)" }
  if ($messages) { $out += "|irq_message_count=$(Sanitize-AeroMarkerValue $messages)" }
  if ($vectors) { $out += "|irq_vectors=$(Sanitize-AeroMarkerValue $vectors)" }
  if ($msiVector) { $out += "|msi_vector=$(Sanitize-AeroMarkerValue $msiVector)" }
  if ($msixConfigVector) { $out += "|msix_config_vector=$(Sanitize-AeroMarkerValue $msixConfigVector)" }
  if ($msixQueueVector) { $out += "|msix_queue_vector=$(Sanitize-AeroMarkerValue $msixQueueVector)" }
  Write-Host $out
}

function Test-AeroVirtioBlkMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk-msix marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|"
  $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  $line = $null
  if ($matches.Count -gt 0) {
    $line = $matches[$matches.Count - 1].Value
  }

  if (($null -eq $line) -and (-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      $line = $last
    }
  }

  if ($null -eq $line) {
    return @{ Ok = $false; Reason = "missing virtio-blk-msix marker (guest selftest too old?)" }
  }

  if ($line -match "\|FAIL(\||$)") {
    return @{ Ok = $false; Reason = "virtio-blk-msix marker reported FAIL" }
  }
  if ($line -match "\|SKIP(\||$)") {
    return @{ Ok = $false; Reason = "virtio-blk-msix marker reported SKIP" }
  }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if (-not $fields.ContainsKey("mode")) {
    return @{ Ok = $false; Reason = "virtio-blk-msix marker missing mode=... field" }
  }

  $mode = [string]$fields["mode"]
  if ($mode -ne "msix") {
    $msgs = "?"
    if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
    return @{ Ok = $false; Reason = "mode=$mode (expected msix) messages=$msgs" }
  }

  return @{ Ok = $true; Reason = "ok" }
}

function Test-AeroVirtioSndMsixMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-msix marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|"
  $matches = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  $line = $null
  if ($matches.Count -gt 0) {
    $line = $matches[$matches.Count - 1].Value
  }

  if (($null -eq $line) -and (-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath)) {
    # Tail truncation fallback: scan the full serial log line-by-line.
    $last = $null
    try {
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $l = $sr.ReadLine()
            if ($null -eq $l) { break }
            $t = $l.Trim()
            if ($t.StartsWith($prefix)) {
              $last = $t
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
    if ($null -ne $last) {
      $line = $last
    }
  }

  if ($null -eq $line) {
    return @{ Ok = $false; Reason = "missing virtio-snd-msix marker (guest selftest too old?)" }
  }

  if ($line -match "\|FAIL(\||$)") {
    return @{ Ok = $false; Reason = "virtio-snd-msix marker reported FAIL" }
  }
  if ($line -match "\|SKIP(\||$)") {
    return @{ Ok = $false; Reason = "virtio-snd-msix marker reported SKIP" }
  }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if (-not $fields.ContainsKey("mode")) {
    return @{ Ok = $false; Reason = "virtio-snd-msix marker missing mode=... field" }
  }

  $mode = [string]$fields["mode"]
  if ($mode -ne "msix") {
    $msgs = "?"
    if ($fields.ContainsKey("messages")) { $msgs = [string]$fields["messages"] }
    return @{ Ok = $false; Reason = "mode=$mode (expected msix) messages=$msgs" }
  }

  return @{ Ok = $true; Reason = "ok" }
}

function Try-EmitAeroVirtioNetLargeMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-net marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-net|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if (-not (
      $fields.ContainsKey("large_ok") -or
      $fields.ContainsKey("large_bytes") -or
      $fields.ContainsKey("large_mbps") -or
      $fields.ContainsKey("large_fnv1a64") -or
      $fields.ContainsKey("upload_ok") -or
      $fields.ContainsKey("upload_bytes") -or
      $fields.ContainsKey("upload_mbps") -or
      $fields.ContainsKey("msi") -or
      $fields.ContainsKey("msi_messages")
    )) {
    return
  }

  $status = "INFO"
  # Prefer the overall marker PASS/FAIL token so this stays correct even when the
  # large download passes but the optional large upload fails (TX vs RX stress).
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }
  elseif ($fields.ContainsKey("large_ok") -and $fields["large_ok"] -eq "0") { $status = "FAIL" }
  elseif ($fields.ContainsKey("upload_ok") -and $fields["upload_ok"] -eq "0") { $status = "FAIL" }
  elseif ($fields.ContainsKey("large_ok") -and $fields["large_ok"] -eq "1" -and (-not $fields.ContainsKey("upload_ok") -or $fields["upload_ok"] -eq "1")) { $status = "PASS" }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|$status"
  foreach ($k in @("large_ok", "large_bytes", "large_fnv1a64", "large_mbps", "upload_ok", "upload_bytes", "upload_mbps", "msi", "msi_messages")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioBlkIoMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-blk marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  # Backward compatible: older guest selftests will not include the I/O perf fields. Emit nothing
  # unless we see at least one throughput/byte count key.
  if (-not (
      $fields.ContainsKey("write_bytes") -or
      $fields.ContainsKey("write_mbps") -or
      $fields.ContainsKey("read_bytes") -or
      $fields.ContainsKey("read_mbps")
    )) {
    return
  }

  $status = "INFO"
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }
  elseif (($fields.ContainsKey("write_ok") -and $fields["write_ok"] -eq "0") -or ($fields.ContainsKey("flush_ok") -and $fields["flush_ok"] -eq "0") -or ($fields.ContainsKey("read_ok") -and $fields["read_ok"] -eq "0")) {
    $status = "FAIL"
  } elseif (($fields.ContainsKey("write_ok") -and $fields["write_ok"] -eq "1") -and ($fields.ContainsKey("flush_ok") -and $fields["flush_ok"] -eq "1") -and ($fields.ContainsKey("read_ok") -and $fields["read_ok"] -eq "1")) {
    $status = "PASS"
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|$status"
  foreach ($k in @("write_ok", "write_bytes", "write_mbps", "flush_ok", "read_ok", "read_bytes", "read_mbps")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioIrqMarkerFromTestMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Guest per-test marker name (e.g. virtio-net, virtio-snd, virtio-input).
    [Parameter(Mandatory = $true)] [string]$Device,
    # Host marker token (e.g. VIRTIO_NET_IRQ).
    [Parameter(Mandatory = $true)] [string]$HostMarker,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|$Device|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $fields = @{}
  foreach ($tok in $line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $irqKeys = @($fields.Keys | Where-Object { $_.StartsWith("irq_") })
  if ($irqKeys.Count -eq 0) { return }

  $status = "INFO"
  if ($line -match "\|FAIL(\||$)") { $status = "FAIL" }
  elseif ($line -match "\|PASS(\||$)") { $status = "PASS" }

  $out = "AERO_VIRTIO_WIN7_HOST|$HostMarker|$status"

  # Keep ordering stable for log scraping.
  foreach ($k in @("irq_mode", "irq_message_count")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($irqKeys | Where-Object { $_ -ne "irq_mode" -and $_ -ne "irq_message_count" } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndCaptureMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-capture marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|$status"

  # The guest SKIP marker often uses a plain token (e.g. `...|SKIP|endpoint_missing`) rather than
  # a `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  foreach ($k in @("method", "frames", "non_silence", "silence_only", "reason", "hr")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndDuplexMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-duplex marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|$status"

  if (($status -eq "SKIP" -or $status -eq "FAIL") -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq $status) {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $fields["reason"] = $reasonTok
          }
        }
        break
      }
    }
  }

  foreach ($k in @("frames", "non_silence", "reason", "hr")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndBufferLimitsMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-buffer-limits marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }

  $toks = $line.Split("|")
  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|$status"
  foreach ($k in @("mode", "expected_failure", "buffer_bytes", "init_hr", "hr", "reason")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioSndEventqMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-eventq marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $toks = $line.Split("|")

  # The guest marker is informational and typically uses INFO or SKIP, but accept PASS/FAIL
  # if a future guest selftest implementation uses them.
  $status = "INFO"
  foreach ($t in $toks) {
    if ($t.Trim() -eq "FAIL") { $status = "FAIL"; break }
    if ($t.Trim() -eq "PASS") { $status = "PASS"; break }
    if ($t.Trim() -eq "SKIP") { $status = "SKIP"; break }
    if ($t.Trim() -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|$status"

  # The guest SKIP marker uses a plain token (e.g. `...|SKIP|device_missing`) rather than a
  # `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
  if ($status -eq "SKIP" -and (-not $fields.ContainsKey("reason"))) {
    for ($i = 0; $i -lt $toks.Count; $i++) {
      if ($toks[$i].Trim() -eq "SKIP") {
        if ($i + 1 -lt $toks.Count) {
          $reasonTok = $toks[$i + 1].Trim()
          if (-not [string]::IsNullOrEmpty($reasonTok) -and ($reasonTok.IndexOf("=") -lt 0)) {
            $out += "|reason=$(Sanitize-AeroMarkerValue $reasonTok)"
          }
        }
        break
      }
    }
  }

  # Keep ordering stable for log scraping.
  $ordered = @(
    "completions",
    "parsed",
    "short",
    "unknown",
    "jack_connected",
    "jack_disconnected",
    "pcm_period",
    "xrun",
    "ctl_notify"
  )
  $orderedSet = @{}
  foreach ($k in $ordered) { $orderedSet[$k] = $true }

  foreach ($k in $ordered) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }

  foreach ($k in ($fields.Keys | Where-Object { (-not $orderedSet.ContainsKey($_)) -and $_ -ne "reason" } | Sort-Object)) {
    $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
  }

  Write-Host $out
}

function Try-EmitAeroVirtioSndFormatMarker {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain the virtio-snd-format marker (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  $prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|"
  $line = Try-ExtractLastAeroMarkerLine -Tail $Tail -Prefix $prefix -SerialLogPath $SerialLogPath
  if ($null -eq $line) { return }
  $toks = $line.Split("|")

  $status = "INFO"
  foreach ($t in $toks) {
    $tt = $t.Trim()
    if ($tt -eq "FAIL") { $status = "FAIL"; break }
    if ($tt -eq "PASS") { $status = "PASS"; break }
    if ($tt -eq "SKIP") { $status = "SKIP"; break }
    if ($tt -eq "INFO") { $status = "INFO" }
  }

  $fields = @{}
  foreach ($tok in $toks) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx).Trim()
    $v = $tok.Substring($idx + 1).Trim()
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  $out = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|$status"
  foreach ($k in @("render", "capture")) {
    if ($fields.ContainsKey($k)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
  }
  Write-Host $out
}

function Try-EmitAeroVirtioIrqDiagnosticsMarkers {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    # Optional: if provided, fall back to parsing the full serial log when the rolling tail buffer does not
    # contain any virtio-*-irq markers (e.g. because the tail was truncated).
    [Parameter(Mandatory = $false)] [string]$SerialLogPath = ""
  )

  # Guest selftest may emit interrupt mode diagnostics markers:
  #   virtio-<dev>-irq|INFO|mode=msix|vectors=...
  #   virtio-<dev>-irq|WARN|mode=intx|reason=...
  #
  # These are informational by default and do not affect PASS/FAIL; the harness
  # re-emits them as host-side markers for log scraping/diagnostics.
  $parseText = {
    param([Parameter(Mandatory = $true)] [string]$Text)
    $map = @{}
    foreach ($line in ($Text -split "`r`n|`n|`r")) {
      if ($line -match "^\s*virtio-(.+)-irq\|(INFO|WARN)(?:\|(.*))?$") {
        $dev = $Matches[1]
        $level = $Matches[2]
        $rest = $Matches[3]
        $fields = @{}
        $extras = @()
        if (-not [string]::IsNullOrEmpty($rest)) {
          foreach ($tok in $rest.Split("|")) {
            if ([string]::IsNullOrEmpty($tok)) { continue }
            $idx = $tok.IndexOf("=")
            if ($idx -gt 0) {
              $k = $tok.Substring(0, $idx).Trim()
              $v = $tok.Substring($idx + 1).Trim()
              if (-not [string]::IsNullOrEmpty($k)) {
                $fields[$k] = $v
              }
            } else {
              $extras += $tok.Trim()
            }
          }
        }
        if ($extras.Count -gt 0) {
          $fields["msg"] = ($extras -join "|")
        }
        $map[$dev] = @{
          Level = $level
          Fields = $fields
        }
      }
    }
    return $map
  }

  $byDev = & $parseText $Tail

  # If a serial log path is provided, optionally merge markers from the full file so early
  # virtio-*-irq lines are not lost when the rolling tail buffer is truncated.
  #
  # We only do this when:
  # - no markers were found in the tail (e.g. guest printed them early), OR
  # - the tail buffer hit the cap (likely truncated).
  $tailWasTruncated = $Tail.Length -ge 131072
  if ((-not [string]::IsNullOrEmpty($SerialLogPath)) -and (Test-Path -LiteralPath $SerialLogPath) -and ($byDev.Count -eq 0 -or $tailWasTruncated)) {
    try {
      # Avoid reading the entire file into memory; scan line-by-line and keep the last marker per device.
      $fs = [System.IO.File]::Open($SerialLogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
      try {
        $sr = [System.IO.StreamReader]::new($fs, [System.Text.Encoding]::UTF8, $true, 4096, $true)
        try {
          while ($true) {
            $line = $sr.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match "^\s*virtio-(.+)-irq\|(INFO|WARN)(?:\|(.*))?$") {
              $dev = $Matches[1]
              $level = $Matches[2]
              $rest = $Matches[3]
              $fields = @{}
              $extras = @()
              if (-not [string]::IsNullOrEmpty($rest)) {
                foreach ($tok in $rest.Split("|")) {
                  if ([string]::IsNullOrEmpty($tok)) { continue }
                  $idx = $tok.IndexOf("=")
                  if ($idx -gt 0) {
                    $k = $tok.Substring(0, $idx).Trim()
                    $v = $tok.Substring($idx + 1).Trim()
                    if (-not [string]::IsNullOrEmpty($k)) {
                      $fields[$k] = $v
                    }
                  } else {
                    $extras += $tok.Trim()
                  }
                }
              }
              if ($extras.Count -gt 0) {
                $fields["msg"] = ($extras -join "|")
              }
              $byDev[$dev] = @{
                Level = $level
                Fields = $fields
              }
            }
          }
        } finally {
          $sr.Dispose()
        }
      } finally {
        $fs.Dispose()
      }
    } catch { }
  }

  foreach ($dev in ($byDev.Keys | Sort-Object)) {
    $info = $byDev[$dev]
    $level = $info.Level
    $fields = $info.Fields
    $name = "VIRTIO_$($dev.ToUpperInvariant().Replace('-', '_'))_IRQ_DIAG"
    $out = "AERO_VIRTIO_WIN7_HOST|$name|$level"
    foreach ($k in ($fields.Keys | Sort-Object)) {
      $out += "|$k=$(Sanitize-AeroMarkerValue $fields[$k])"
    }
    Write-Host $out
  }
}

function Normalize-AeroVirtioIrqMode {
  param(
    [Parameter(Mandatory = $true)] [string]$Mode
  )

  $m = ([string]$Mode).Trim().ToLowerInvariant().Replace("_", "-")
  if ($m -eq "msi-x") { return "msix" }
  return $m
}

function Get-AeroVirtioIrqModeFamily {
  param(
    [Parameter(Mandatory = $true)] [string]$Mode
  )

  $m = Normalize-AeroVirtioIrqMode $Mode
  if ($m -eq "intx") { return "intx" }
  if ($m -eq "msi" -or $m -eq "msix") { return "msi" }
  return ""
}

function Get-AeroIrqModeFromAeroMarkerLine {
  param(
    [Parameter(Mandatory = $true)] [string]$Line
  )

  $fields = @{}
  foreach ($tok in $Line.Split("|")) {
    $idx = $tok.IndexOf("=")
    if ($idx -le 0) { continue }
    $k = $tok.Substring(0, $idx)
    $v = $tok.Substring($idx + 1)
    if (-not [string]::IsNullOrEmpty($k)) {
      $fields[$k] = $v
    }
  }

  if ($fields.ContainsKey("irq_mode")) { return $fields["irq_mode"] }
  if ($fields.ContainsKey("mode")) {
    $m = $fields["mode"]
    if (-not [string]::IsNullOrEmpty((Get-AeroVirtioIrqModeFamily $m))) { return $m }
  }
  if ($fields.ContainsKey("interrupt_mode")) {
    $m = $fields["interrupt_mode"]
    if (-not [string]::IsNullOrEmpty((Get-AeroVirtioIrqModeFamily $m))) { return $m }
  }

  return $null
}

function Get-AeroVirtioIrqMode {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string]$Device
  )

  $dev = $Device
  if ($dev.StartsWith("virtio-")) { $dev = $dev.Substring(7) }

  # Prefer standalone guest IRQ diagnostics markers:
  #   virtio-<dev>-irq|INFO/WARN|mode=intx/msi/msix|...
  $re = [regex]::new("(?m)^\s*virtio-" + [regex]::Escape($dev) + "-irq\|(INFO|WARN)(?:\|(?<rest>.*))?$")
  $matches = $re.Matches($Tail)
  if ($matches.Count -gt 0) {
    $rest = $matches[$matches.Count - 1].Groups["rest"].Value
    if (-not [string]::IsNullOrEmpty($rest)) {
      foreach ($tok in $rest.Split("|")) {
        if ($tok -match "^mode=(.+)$") {
          return $Matches[1]
        }
      }
    }
  }

  # virtio-blk has additional marker variants in some guest builds.
  if ($Device -eq "virtio-blk") {
    foreach ($prefix in @(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-irq|",
        "AERO_VIRTIO_SELFTEST|MARKER|virtio-blk-irq|",
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|IRQ|",
        "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|INFO|"
      )) {
      $mm = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
      if ($mm.Count -gt 0) {
        $line = $mm[$mm.Count - 1].Value
        $mode = Get-AeroIrqModeFromAeroMarkerLine $line
        if (-not [string]::IsNullOrEmpty($mode)) { return $mode }
      }
    }
  }

  # Fall back to per-test marker fields when present.
  $prefix = "AERO_VIRTIO_SELFTEST|TEST|$Device|"
  $mm = [regex]::Matches($Tail, [regex]::Escape($prefix) + "[^`r`n]*")
  if ($mm.Count -gt 0) {
    $line = $mm[$mm.Count - 1].Value
    $mode = Get-AeroIrqModeFromAeroMarkerLine $line
    if (-not [string]::IsNullOrEmpty($mode)) { return $mode }
  }

  return $null
}

function Test-AeroVirtioIrqModeEnforcement {
  param(
    [Parameter(Mandatory = $true)] [string]$Tail,
    [Parameter(Mandatory = $true)] [string[]]$Devices,
    [Parameter(Mandatory = $true)] [ValidateSet("intx", "msi")] [string]$Expected
  )

  foreach ($dev in $Devices) {
    $mode = Get-AeroVirtioIrqMode -Tail $Tail -Device $dev
    $got = ""
    if (-not [string]::IsNullOrEmpty($mode)) {
      $got = Normalize-AeroVirtioIrqMode $mode
    }
    $family = ""
    if (-not [string]::IsNullOrEmpty($got)) {
      $family = Get-AeroVirtioIrqModeFamily $got
    }

    if ([string]::IsNullOrEmpty($family)) {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $(if ([string]::IsNullOrEmpty($got)) { "unknown" } else { $got }) }
    }
    if ($Expected -eq "intx" -and $family -ne "intx") {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $got }
    }
    if ($Expected -eq "msi" -and $family -ne "msi") {
      return @{ Ok = $false; Device = $dev; Expected = $Expected; Got = $got }
    }
  }

  return @{ Ok = $true }
}

function Invoke-AeroVirtioSndWavVerification {
  param(
    [Parameter(Mandatory = $true)] [string]$WavPath,
    [Parameter(Mandatory = $true)] [int]$PeakThreshold,
    [Parameter(Mandatory = $true)] [int]$RmsThreshold
  )

  $markerPrefix = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_WAV"

  try {
    if (-not (Test-Path -LiteralPath $WavPath)) {
      Write-Host "$markerPrefix|FAIL|reason=missing_wav_file|path=$(Sanitize-AeroMarkerValue $WavPath)"
      return $false
    }

    $fi = Get-Item -LiteralPath $WavPath
    if ($fi.Length -le 0) {
      Write-Host "$markerPrefix|FAIL|reason=empty_wav_file|path=$(Sanitize-AeroMarkerValue $WavPath)"
      return $false
    }

    $fs = [System.IO.File]::Open($WavPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    $br = [System.IO.BinaryReader]::new($fs, [System.Text.Encoding]::ASCII, $true)

    try {
      $riffBytes = $br.ReadBytes(4)
      if ($riffBytes.Length -lt 4) { throw "wav file too small for RIFF header" }
      $riff = [System.Text.Encoding]::ASCII.GetString($riffBytes)
      if ($riff -ne "RIFF") { throw "missing RIFF header" }
      $null = $br.ReadUInt32() # file size (ignored)
      $waveBytes = $br.ReadBytes(4)
      if ($waveBytes.Length -lt 4) { throw "wav file too small for WAVE header" }
      $wave = [System.Text.Encoding]::ASCII.GetString($waveBytes)
      if ($wave -ne "WAVE") { throw "missing WAVE form type" }

      $fmtFound = $false
      $dataFound = $false
      $formatTag = 0
      $channels = 0
      $sampleRate = 0
      $blockAlign = 0
      $bitsPerSample = 0
      $subformatHex = ""
      $dataOffset = 0L
      $dataSize = 0L

      while (($fs.Position + 8) -le $fs.Length) {
        $chunkIdBytes = $br.ReadBytes(4)
        if ($chunkIdBytes.Length -lt 4) { break }
        $chunkId = [System.Text.Encoding]::ASCII.GetString($chunkIdBytes)
        $chunkSize = [long]$br.ReadUInt32()
        $chunkDataStart = $fs.Position
        $remaining = $fs.Length - $chunkDataStart
        if ($chunkSize -gt $remaining) { $chunkSize = $remaining }

        if ($chunkId -eq "fmt ") {
          if ($chunkSize -lt 16) { throw "fmt chunk too small" }
          $formatTag = [int]$br.ReadUInt16()
          $channels = [int]$br.ReadUInt16()
          $sampleRate = [int]$br.ReadUInt32()
          $null = $br.ReadUInt32() # avg bytes/sec (ignored)
          $blockAlign = [int]$br.ReadUInt16()
          $bitsPerSample = [int]$br.ReadUInt16()
          $fmtFound = $true
          # WAVE_FORMAT_EXTENSIBLE appends an extension that contains the SubFormat GUID.
          if ($formatTag -eq 0xFFFE -and $chunkSize -ge 40) {
            $null = $br.ReadUInt16() # cbSize
            $null = $br.ReadUInt16() # valid bits per sample
            $null = $br.ReadUInt32() # channel mask
            $sub = $br.ReadBytes(16)
            if ($sub.Length -eq 16) {
              $subformatHex = ([System.BitConverter]::ToString($sub).Replace("-", "").ToLowerInvariant())
            }
            $skip = $chunkSize - 40
            if ($skip -gt 0) { $fs.Seek($skip, [System.IO.SeekOrigin]::Current) | Out-Null }
          } else {
            $skip = $chunkSize - 16
            if ($skip -gt 0) { $fs.Seek($skip, [System.IO.SeekOrigin]::Current) | Out-Null }
          }
        } elseif ($chunkId -eq "data") {
          # If QEMU is killed hard, it may never rewrite the data chunk size (placeholder 0).
          # Recover by treating the rest of the file as audio data when it doesn't look like another chunk header.
          $effectiveSize = $chunkSize
          if ($chunkSize -eq 0 -and $remaining -gt 0) {
            $peekLen = 8
            if ($remaining -lt 8) { $peekLen = [int]$remaining }
            $peek = $br.ReadBytes($peekLen)
            $fs.Seek(-$peek.Length, [System.IO.SeekOrigin]::Current) | Out-Null

            $looksLikeChunk = $false
            if ($peek.Length -ge 8) {
              $printable = $true
              for ($i = 0; $i -lt 4; $i++) {
                $b = $peek[$i]
                if ($b -lt 0x20 -or $b -gt 0x7E) { $printable = $false; break }
              }
              if ($printable) {
                $nextSize = [System.BitConverter]::ToUInt32($peek, 4)
                if ($nextSize -le ($remaining - 8)) { $looksLikeChunk = $true }
              }
            }

            if (-not $looksLikeChunk) { $effectiveSize = $remaining }
          }

          if (-not $dataFound -or ($dataSize -eq 0 -and $effectiveSize -gt 0)) {
            $dataOffset = [long]$chunkDataStart
            $dataSize = [long]$effectiveSize
            $dataFound = $true
          }
          $fs.Seek($effectiveSize, [System.IO.SeekOrigin]::Current) | Out-Null
        } else {
          $fs.Seek($chunkSize, [System.IO.SeekOrigin]::Current) | Out-Null
        }

        if (($chunkSize % 2) -eq 1 -and $fs.Position -lt $fs.Length) {
          $fs.Seek(1, [System.IO.SeekOrigin]::Current) | Out-Null
        }
      }

      if (-not $fmtFound) { throw "missing fmt chunk" }
      if (-not $dataFound) { throw "missing data chunk" }

      $pcmGuidHex = "0100000000001000800000aa00389b71"
      $floatGuidHex = "0300000000001000800000aa00389b71"

      $kind = ""
      if ($formatTag -eq 1) {
        $kind = "pcm"
      } elseif ($formatTag -eq 3) {
        $kind = "float"
      } elseif ($formatTag -eq 0xFFFE) {
        if ([string]::IsNullOrEmpty($subformatHex)) {
          Write-Host "$markerPrefix|FAIL|reason=unsupported_extensible_missing_subformat"
          return $false
        }
        if ($subformatHex -eq $pcmGuidHex) {
          $kind = "pcm"
        } elseif ($subformatHex -eq $floatGuidHex) {
          $kind = "float"
        } else {
          Write-Host "$markerPrefix|FAIL|reason=unsupported_extensible_subformat_$(Sanitize-AeroMarkerValue $subformatHex)"
          return $false
        }
      } else {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_format_tag_$formatTag"
        return $false
      }

      if ($kind -eq "pcm" -and ($bitsPerSample -ne 8 -and $bitsPerSample -ne 16 -and $bitsPerSample -ne 24 -and $bitsPerSample -ne 32)) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_bits_per_sample_$bitsPerSample"
        return $false
      }
      if ($kind -eq "float" -and $bitsPerSample -ne 32) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_bits_per_sample_$bitsPerSample"
        return $false
      }
      if ($dataSize -le 0) {
        Write-Host "$markerPrefix|FAIL|reason=missing_or_empty_data_chunk"
        return $false
      }

      $fs.Seek($dataOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
      $peakF = 0.0
      $sumSq = 0.0
      $sampleValues = 0L
      $remainingData = $dataSize
      $buf = New-Object byte[] 65536
      $carry = $null
      $sampleBytes = [int]($bitsPerSample / 8)

      while ($remainingData -gt 0) {
        $toRead = [int][Math]::Min($buf.Length, $remainingData)
        $n = $fs.Read($buf, 0, $toRead)
        if ($n -le 0) { break }
        $remainingData -= $n

        # Build a contiguous byte[] for the current chunk (plus any carry from the previous read).
        if ($null -ne $carry) {
          $data = New-Object byte[] ($carry.Length + $n)
          [System.Buffer]::BlockCopy($carry, 0, $data, 0, $carry.Length)
          [System.Buffer]::BlockCopy($buf, 0, $data, $carry.Length, $n)
          $carry = $null
        } else {
          $data = New-Object byte[] $n
          [System.Buffer]::BlockCopy($buf, 0, $data, 0, $n)
        }

        $dataLen = $data.Length
        if ($dataLen -le 0) { continue }

        $limit = ($dataLen / $sampleBytes) * $sampleBytes
        if ($limit -le 0) {
          $carry = $data
          continue
        }

        $rem = $dataLen - $limit
        if ($rem -gt 0) {
          $carry = New-Object byte[] $rem
          [System.Buffer]::BlockCopy($data, $limit, $carry, 0, $rem)
        }

        for ($i = 0; $i -lt $limit; $i += $sampleBytes) {
          $v = 0.0
          if ($kind -eq "pcm") {
            if ($bitsPerSample -eq 8) {
              $raw = ([int]$data[$i]) - 128
              $v = ([double]$raw / 128.0) * 32767.0
            } elseif ($bitsPerSample -eq 16) {
              $val = ([int]$data[$i]) -bor (([int]$data[$i + 1]) -shl 8)
              if ($val -ge 0x8000) { $val -= 0x10000 }
              $v = [double]$val
            } elseif ($bitsPerSample -eq 24) {
              $val = ([int]$data[$i]) -bor (([int]$data[$i + 1]) -shl 8) -bor (([int]$data[$i + 2]) -shl 16)
              if (($val -band 0x800000) -ne 0) { $val -= 0x1000000 }
              $v = ([double]$val / 8388608.0) * 32767.0
            } elseif ($bitsPerSample -eq 32) {
              $val = [System.BitConverter]::ToInt32($data, $i)
              $v = ([double]$val / 2147483648.0) * 32767.0
            }
          } elseif ($kind -eq "float") {
            $f = [System.BitConverter]::ToSingle($data, $i)
            if ([double]::IsNaN($f) -or [double]::IsInfinity($f)) { continue }
            $v = ([double]$f) * 32767.0
          }

          $absVal = if ($v -lt 0.0) { -$v } else { $v }
          if ($absVal -gt $peakF) { $peakF = $absVal }
          $sumSq += ($v * $v)
          $sampleValues++
        }
      }

      $rms = if ($sampleValues -gt 0) { [Math]::Sqrt($sumSq / [double]$sampleValues) } else { 0.0 }
      $rmsI = [int][Math]::Round($rms)
      $frames = if ($channels -gt 0) { [long]($sampleValues / $channels) } else { [long]$sampleValues }

      $peak = [int][Math]::Round($peakF)
      if ($peak -gt $PeakThreshold -or $rms -gt $RmsThreshold) {
        Write-Host "$markerPrefix|PASS|peak=$peak|rms=$rmsI|samples=$frames|sr=$sampleRate|ch=$channels"
        return $true
      }

      Write-Host "$markerPrefix|FAIL|reason=silent_pcm|peak=$peak|rms=$rmsI|samples=$frames|sr=$sampleRate|ch=$channels"
      return $false
    } finally {
      if ($br) { $br.Dispose() }
      if ($fs) { $fs.Dispose() }
    }
  } catch {
    $reason = Sanitize-AeroMarkerValue ($_.Exception.Message)
    if ([string]::IsNullOrEmpty($reason)) { $reason = "exception" }
    Write-Host "$markerPrefix|FAIL|reason=$reason"
    return $false
  }
}

function Write-AeroQemuStderrTail {
  param(
    [Parameter(Mandatory = $true)] [string]$Path
  )

  if (-not (Test-Path -LiteralPath $Path)) { return }

  $lines = @()
  try {
    $lines = Get-Content -LiteralPath $Path -Tail 200 -ErrorAction SilentlyContinue
  } catch {
    return
  }
  if ($lines.Count -eq 0) { return }

  Write-Host "`n--- QEMU stderr tail ---"
  $lines | ForEach-Object { Write-Host $_ }
}

function Get-AeroFreeTcpPort {
  # Reserve an ephemeral port by binding to 0, then immediately releasing it. This is inherently
  # racy, but good enough for a best-effort QMP channel used only for graceful shutdown.
  $listener = $null
  try {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    return $listener.LocalEndpoint.Port
  } finally {
    if ($listener) { $listener.Stop() }
  }
}

function Try-AeroQmpQuit {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(2)
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $writer.WriteLine('{"execute":"qmp_capabilities"}')
      $null = $reader.ReadLine()
      $writer.WriteLine('{"execute":"quit"}')
      return $true
    } catch {
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  return $false
}

function Read-AeroQmpResponse {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader
  )

  while ($true) {
    $line = $Reader.ReadLine()
    if ($null -eq $line) {
      throw "EOF while waiting for QMP response"
    }
    if ([string]::IsNullOrWhiteSpace($line)) { continue }

    $obj = $null
    try {
      $obj = $line | ConvertFrom-Json -ErrorAction Stop
    } catch {
      continue
    }

    if ($obj.PSObject.Properties.Name -contains "return") { return $obj }
    if ($obj.PSObject.Properties.Name -contains "error") { return $obj }
    # Otherwise, ignore async events and keep reading.
  }
}

function Invoke-AeroQmpCommand {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $true)] $Command
  )

  $Writer.WriteLine(($Command | ConvertTo-Json -Compress -Depth 10))
  $resp = Read-AeroQmpResponse -Reader $Reader
  if ($resp.PSObject.Properties.Name -contains "error") {
    $desc = ""
    try { $desc = [string]$resp.error.desc } catch { }
    if ([string]::IsNullOrEmpty($desc)) { $desc = "unknown" }
    throw "QMP command failed: $desc"
  }
  return $resp
}

function Invoke-AeroQmpInputSendEvent {
  param(
    [Parameter(Mandatory = $true)] [System.IO.StreamWriter]$Writer,
    [Parameter(Mandatory = $true)] [System.IO.StreamReader]$Reader,
    [Parameter(Mandatory = $false)] [string]$Device = "",
    [Parameter(Mandatory = $true)] $Events
  )

  # Prefer targeting the virtio input devices by QOM id (so we don't accidentally exercise PS/2),
  # but fall back to broadcasting the event when QEMU rejects the `device` argument.
  $cmd = @{
    execute   = "input-send-event"
    arguments = @{
      events = $Events
    }
  }
  if (-not [string]::IsNullOrEmpty($Device)) {
    $cmd.arguments.device = $Device
  }

  try {
    $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
    return $Device
  } catch {
    if ([string]::IsNullOrEmpty($Device)) {
      throw
    }

    # Retry without device routing.
    try {
      $cmd.arguments.Remove("device")
      $null = Invoke-AeroQmpCommand -Writer $Writer -Reader $Reader -Command $cmd
      Write-Warning "QMP input-send-event rejected device='$Device'; falling back to broadcast: $($_.Exception.Message)"
      return $null
    } catch {
      throw "QMP input-send-event failed with device='$Device' and without device: $_"
    }
  }
}

function Try-AeroQmpInjectVirtioInputEvents {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt,
    # If set, inject wheel + horizontal wheel (AC Pan) events in addition to motion/click.
    [Parameter(Mandatory = $false)] [switch]$WithWheel,
    # If set, also inject extended virtio-input events (modifiers/buttons/wheel).
    [Parameter(Mandatory = $false)] [bool]$Extended = $false
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $kbdDevice = $script:VirtioInputKeyboardQmpId
      $mouseDevice = $script:VirtioInputMouseQmpId

      # Keyboard: 'a' down/up.
      $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
        @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "a" } } }
      )

      Start-Sleep -Milliseconds 50

      $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
        @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "a" } } }
      )

      Start-Sleep -Milliseconds 50

      # Mouse: move + left click.
      $wantWheel = ([bool]$WithWheel) -or $Extended
      $mouseRelEvents = @(
        @{ type = "rel"; data = @{ axis = "x"; value = 10 } },
        @{ type = "rel"; data = @{ axis = "y"; value = 5 } }
      )
      if ($wantWheel) {
        $mouseRelEvents += @(
          @{ type = "rel"; data = @{ axis = "wheel"; value = 1 } },
          @{ type = "rel"; data = @{ axis = "hscroll"; value = -2 } }
        )
      }
      try {
        $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events $mouseRelEvents
      } catch {
        if (-not $wantWheel) { throw }
        $errWheelHscroll = ""
        try { $errWheelHscroll = [string]$_.Exception.Message } catch { }

        # Some QEMU builds use alternate axis names for scroll wheels. Try a best-effort matrix of
        # axis pairs before failing.
        $errors = @{}
        $errors["wheel+hscroll"] = $errWheelHscroll
        $attempts = @(
          @{ name = "wheel+hwheel"; axisMap = @{ "hscroll" = "hwheel" } },
          @{ name = "vscroll+hscroll"; axisMap = @{ "wheel" = "vscroll" } },
          @{ name = "vscroll+hwheel"; axisMap = @{ "wheel" = "vscroll"; "hscroll" = "hwheel" } }
        )

        $ok = $false
        foreach ($att in $attempts) {
          $evs = @()
          foreach ($ev in $mouseRelEvents) {
            if ($ev.type -eq "rel" -and $att.axisMap.ContainsKey($ev.data.axis)) {
              $evs += @{ type = "rel"; data = @{ axis = $att.axisMap[$ev.data.axis]; value = $ev.data.value } }
            } else {
              $evs += $ev
            }
          }

          try {
            $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events $evs
            $ok = $true
            break
          } catch {
            $msg = ""
            try { $msg = [string]$_.Exception.Message } catch { }
            $errors[$att.name] = $msg
            continue
          }
        }

        if (-not $ok) {
          $errorsText = ""
          try {
            $errorsText = ($errors.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join "; "
          } catch {
            try { $errorsText = [string]$errors } catch { $errorsText = "<unavailable>" }
          }
          throw "QMP input-send-event failed while injecting scroll for -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel or -WithInputEventsExtended/-WithInputEventsExtra. Upgrade QEMU or omit those flags. errors=[$errorsText]"
        }
      }

      Start-Sleep -Milliseconds 50

      $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
        @{ type = "btn"; data = @{ down = $true; button = "left" } }
      )

      Start-Sleep -Milliseconds 50

      $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
        @{ type = "btn"; data = @{ down = $false; button = "left" } }
      )

      if ($Extended) {
        Start-Sleep -Milliseconds 50

        # Keyboard: modifiers + function key.
        $kbdExtra = @(
          # Shift + b.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "shift" } } },
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "b" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "b" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "shift" } } },
          # Ctrl.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "ctrl" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "ctrl" } } },
          # Alt.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "alt" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "alt" } } },
          # Function key.
          @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = "f1" } } },
          @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = "f1" } } }
        )
        foreach ($evt in $kbdExtra) {
          $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @($evt)
          Start-Sleep -Milliseconds 50
        }

        # Mouse: side/extra buttons.
        $mouseExtra = @(
          @{ type = "btn"; data = @{ down = $true; button = "side" } },
          @{ type = "btn"; data = @{ down = $false; button = "side" } },
          @{ type = "btn"; data = @{ down = $true; button = "extra" } },
          @{ type = "btn"; data = @{ down = $false; button = "extra" } }
        )
        foreach ($evt in $mouseExtra) {
          $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @($evt)
          Start-Sleep -Milliseconds 50
        }
      }

      $kbdMode = if ([string]::IsNullOrEmpty($kbdDevice)) { "broadcast" } else { "device" }
      $mouseMode = if ([string]::IsNullOrEmpty($mouseDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=$Attempt|kbd_mode=$kbdMode|mouse_mode=$mouseMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=$Attempt|reason=$reason"
  return $false
}

function Try-AeroQmpInjectVirtioInputMediaKeys {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt,
    # QMP QKeyCode qcode to send (default: volumeup). The guest selftest currently validates VolumeUp.
    [Parameter(Mandatory = $false)] [string]$Qcode = "volumeup"
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $kbdDevice = $script:VirtioInputKeyboardQmpId

      # Media key: press + release.
      $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
        @{ type = "key"; data = @{ down = $true; key = @{ type = "qcode"; data = $Qcode } } }
      )

      Start-Sleep -Milliseconds 50

      $kbdDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $kbdDevice -Events @(
        @{ type = "key"; data = @{ down = $false; key = @{ type = "qcode"; data = $Qcode } } }
      )

      $kbdMode = if ([string]::IsNullOrEmpty($kbdDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt=$Attempt|kbd_mode=$kbdMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt=$Attempt|reason=$reason"
  return $false
}

function Try-AeroQmpInjectVirtioInputTabletEvents {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    # Outer retry attempt number (1-based). Included in the emitted host marker so log scraping can
    # correlate guest READY/PASS timing with host injection attempts.
    [Parameter(Mandatory = $true)] [int]$Attempt
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $tabletDevice = $script:VirtioInputTabletQmpId

      # Deterministic absolute-pointer move + click sequence.
      #
      # This must match the guest selftest expectations in aero-virtio-selftest.exe.
      # Reset move (0,0) to avoid "no-op" repeats.
      $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
        @{ type = "abs"; data = @{ axis = "x"; value = 0 } },
        @{ type = "abs"; data = @{ axis = "y"; value = 0 } }
      )

      Start-Sleep -Milliseconds 50

      # Target move.
      $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
        @{ type = "abs"; data = @{ axis = "x"; value = 10000 } },
        @{ type = "abs"; data = @{ axis = "y"; value = 20000 } }
      )

      Start-Sleep -Milliseconds 50

      # Left click down/up.
      $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
        @{ type = "btn"; data = @{ down = $true; button = "left" } }
      )

      Start-Sleep -Milliseconds 50

      $tabletDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $tabletDevice -Events @(
        @{ type = "btn"; data = @{ down = $false; button = "left" } }
      )

      $tabletMode = if ([string]::IsNullOrEmpty($tabletDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt=$Attempt|tablet_mode=$tabletMode"
      return $true
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  $reason = "timeout"
  if (-not [string]::IsNullOrEmpty($lastErr)) {
    $reason = Sanitize-AeroMarkerValue $lastErr
  }
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=$Attempt|reason=$reason"
  return $false
}

function Convert-AeroPciInt {
  param(
    [Parameter(Mandatory = $false)] $Value
  )
  if ($null -eq $Value) { return $null }
  if ($Value -is [int] -or $Value -is [long]) { return [int]$Value }

  $s = ([string]$Value).Trim()
  if ([string]::IsNullOrEmpty($s)) { return $null }
  if ($s.StartsWith("0x") -or $s.StartsWith("0X")) {
    try { return [int]([Convert]::ToInt32($s.Substring(2), 16)) } catch { return $null }
  }

  $n = 0
  if ([int]::TryParse($s, [ref]$n)) { return $n }

  # Fallback: bare hex without a prefix (e.g. "1af4").
  try { return [int]([Convert]::ToInt32($s, 16)) } catch { return $null }
}

function Format-AeroPciBdf {
  param(
    [Parameter(Mandatory = $false)] [Nullable[int]]$Bus,
    [Parameter(Mandatory = $false)] [Nullable[int]]$Slot,
    [Parameter(Mandatory = $false)] [Nullable[int]]$Function
  )
  if (($null -eq $Bus) -or ($null -eq $Slot) -or ($null -eq $Function)) { return "?:?.?" }
  return ("{0:x2}:{1:x2}.{2}" -f $Bus, $Slot, $Function)
}

function Get-AeroPciIdsFromQueryPci {
  param(
    [Parameter(Mandatory = $true)] $QueryPciReturn
  )

  $ids = @()
  if ($null -eq $QueryPciReturn) { return $ids }

  foreach ($bus in $QueryPciReturn) {
    $busNum = Convert-AeroPciInt $bus.bus
    $devs = $bus.devices
    if ($null -eq $devs) { continue }
    foreach ($dev in $devs) {
      $vendor = Convert-AeroPciInt $dev.vendor_id
      $device = Convert-AeroPciInt $dev.device_id
      if (($null -eq $vendor) -or ($null -eq $device)) { continue }

      $rev = Convert-AeroPciInt $dev.revision
      $subVendor = Convert-AeroPciInt $dev.subsystem_vendor_id
      $subId = Convert-AeroPciInt $dev.subsystem_id

      $devBus = Convert-AeroPciInt $dev.bus
      if ($null -eq $devBus) { $devBus = $busNum }
      $slot = Convert-AeroPciInt $dev.slot
      $function = Convert-AeroPciInt $dev.function

      $ids += [pscustomobject]@{
        VendorId          = $vendor
        DeviceId          = $device
        Revision          = $rev
        SubsystemVendorId = $subVendor
        SubsystemId       = $subId
        Bus               = $devBus
        Slot              = $slot
        Function          = $function
      }
    }
  }
  return $ids
}

function Format-AeroPciIdSummary {
  param(
    [Parameter(Mandatory = $true)] $Ids
  )

  $keys = @{}
  foreach ($d in $Ids) {
    $ven = Convert-AeroPciInt $d.VendorId
    $dev = Convert-AeroPciInt $d.DeviceId
    $rev = Convert-AeroPciInt $d.Revision
    if (($null -eq $ven) -or ($null -eq $dev)) { continue }
    $revText = if ($null -eq $rev) { "??" } else { "{0:x2}" -f $rev }
    $k = "{0:x4}:{1:x4}@{2}" -f $ven, $dev, $revText
    $keys[$k] = $true
  }

  # Keys are formatted with fixed-width hex fields (e.g. "1af4:1041@01"), so lexicographic sorting
  # is stable enough and keeps this compatible with Windows PowerShell 5.1 (no Sort-Object -Stable).
  return (($keys.Keys | Sort-Object) -join ",")
}

function Format-AeroPciIdDump {
  param(
    [Parameter(Mandatory = $true)] $Ids,
    [Parameter(Mandatory = $false)] [int]$MaxLines = 32
  )

  $lines = @()
  foreach ($d in ($Ids | Sort-Object VendorId, DeviceId, Bus, Slot, Function)) {
    $ven = Convert-AeroPciInt $d.VendorId
    $dev = Convert-AeroPciInt $d.DeviceId
    if (($null -eq $ven) -or ($null -eq $dev)) { continue }

    $bdf = Format-AeroPciBdf -Bus $d.Bus -Slot $d.Slot -Function $d.Function
    $revText = if ($null -eq $d.Revision) { "?" } else { "0x{0:x2}" -f ([int]$d.Revision) }
    $subText = "?:?"
    if (($null -ne $d.SubsystemVendorId) -and ($null -ne $d.SubsystemId)) {
      $subText = "0x{0:x4}:0x{1:x4}" -f ([int]$d.SubsystemVendorId), ([int]$d.SubsystemId)
    }
    $lines += ("$bdf 0x{0:x4}:0x{1:x4} subsys=$subText rev=$revText" -f $ven, $dev)
  }

  if ($lines.Count -eq 0) { return "  (no devices parsed from query-pci output)" }

  if ($lines.Count -gt $MaxLines) {
    $extra = $lines.Count - $MaxLines
    $lines = @($lines[0..($MaxLines - 1)]) + "... ($extra more)"
  }

  return (($lines | ForEach-Object { "  $_" }) -join "`n")
}

function Test-AeroQmpVirtioPciPreflight {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $false)] [bool]$VirtioTransitional = $false,
    [Parameter(Mandatory = $false)] [bool]$WithVirtioSnd = $false,
    [Parameter(Mandatory = $false)] [bool]$WithVirtioTablet = $false
  )

  $deadline = [DateTime]::UtcNow.AddSeconds(5)
  $lastErr = ""
  while ([DateTime]::UtcNow -lt $deadline) {
    $client = $null
    try {
      $client = [System.Net.Sockets.TcpClient]::new()
      $client.ReceiveTimeout = 2000
      $client.SendTimeout = 2000
      $client.Connect($Host, $Port)

      $stream = $client.GetStream()
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
      $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
      $writer.NewLine = "`n"
      $writer.AutoFlush = $true

      # Greeting.
      $null = $reader.ReadLine()
      $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

      $query = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "query-pci" }).return
      $ids = Get-AeroPciIdsFromQueryPci -QueryPciReturn $query
      $virtio = @($ids | Where-Object { $_.VendorId -eq 0x1AF4 })

      if ($virtio.Count -eq 0) {
        $dump = Format-AeroPciIdDump -Ids $ids
        return @{
          Ok     = $false
          Reason = "QMP query-pci did not report any virtio PCI devices (expected vendor_id=0x1AF4).`nquery-pci devices:`n$dump"
        }
      }

      if ($VirtioTransitional) {
        $summary = Format-AeroPciIdSummary -Ids $virtio
        Write-Host "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=transitional|vendor=1af4|devices=$(Sanitize-AeroMarkerValue $summary)"
        return @{ Ok = $true }
      }

      $expectedCounts = @{}
      $expectedCounts[0x1041] = 1
      $expectedCounts[0x1042] = 1
      $expectedCounts[0x1052] = $(if ($WithVirtioTablet) { 3 } else { 2 })
      if ($WithVirtioSnd) { $expectedCounts[0x1059] = 1 }

      $missing = @()
      foreach ($devId in ($expectedCounts.Keys | Sort-Object)) {
        $want = [int]$expectedCounts[$devId]
        $have = @($virtio | Where-Object { $_.DeviceId -eq $devId }).Count
        if ($have -lt $want) {
          $missing += ("DEV_{0:X4} (need>={1}, got={2})" -f $devId, $want, $have)
        }
      }

      $badRev = @(
        $virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) -and ($null -ne $_.Revision) -and $_.Revision -ne 0x01 }
      )
      $missingRev = @(
        $virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) -and $null -eq $_.Revision }
      )
      $revProblems = @($badRev + $missingRev)

      if (($missing.Count -gt 0) -or ($revProblems.Count -gt 0)) {
        $summary = Format-AeroPciIdSummary -Ids $virtio
        $dump = Format-AeroPciIdDump -Ids $virtio
        $expectedStr = ($expectedCounts.Keys | Sort-Object | ForEach-Object { "DEV_{0:X4}" -f $_ }) -join "/"
        $reason = "QEMU PCI preflight failed (expected Aero contract v1 virtio PCI IDs).`nExpected (vendor/device/rev): VEN_1AF4 with $expectedStr and REV_01."
        if ($missing.Count -gt 0) {
          $reason += "`nMissing expected device IDs: " + ($missing -join ", ")
        }
        if ($revProblems.Count -gt 0) {
          $revStr = (
            $revProblems |
              Sort-Object VendorId, DeviceId, Bus, Slot, Function |
              ForEach-Object {
                $r = if ($null -eq $_.Revision) { "??" } else { "{0:X2}" -f ([int]$_.Revision) }
                "{0:X4}:{1:X4}@{2}" -f ([int]$_.VendorId), ([int]$_.DeviceId), $r
              }
          ) -join ", "
          $reason += "`nUnexpected revision IDs (expected REV_01): $revStr"
        }
        $reason += "`nDetected virtio devices (from query-pci):`n$dump`nCompact summary: $summary"
        $reason += "`nHint: in contract-v1 mode the harness expects modern-only virtio-pci devices with disable-legacy=on,x-pci-revision=0x01."
        return @{ Ok = $false; Reason = $reason }
      }

      $matched = @($virtio | Where-Object { $expectedCounts.ContainsKey($_.DeviceId) })
      $summary = Format-AeroPciIdSummary -Ids $matched
      Write-Host "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=contract-v1|vendor=1af4|devices=$(Sanitize-AeroMarkerValue $summary)"
      return @{ Ok = $true }
    } catch {
      try { $lastErr = [string]$_.Exception.Message } catch { }
      Start-Sleep -Milliseconds 100
      continue
    } finally {
      if ($client) { $client.Close() }
    }
  }

  if ([string]::IsNullOrEmpty($lastErr)) { $lastErr = "timeout" }
  return @{ Ok = $false; Reason = "failed to connect to QMP for PCI preflight: $(Sanitize-AeroMarkerValue $lastErr)" }
}

function Get-AeroPciMsixInfoFromQueryPci {
  param(
    [Parameter(Mandatory = $true)] $QueryPciReturn
  )

  $infos = @()
  if ($null -eq $QueryPciReturn) { return $infos }

  foreach ($bus in $QueryPciReturn) {
    $busNum = Convert-AeroPciInt $bus.bus
    $devs = $bus.devices
    if ($null -eq $devs) { continue }
    foreach ($dev in $devs) {
      $vendor = Convert-AeroPciInt $dev.vendor_id
      $device = Convert-AeroPciInt $dev.device_id
      if (($null -eq $vendor) -or ($null -eq $device)) { continue }

      $msixEnabled = $null
      try {
        foreach ($cap in $dev.capabilities) {
          $capId = ""
          try { $capId = [string]$cap.id } catch { }
          if (-not [string]::IsNullOrEmpty($capId) -and $capId.ToLowerInvariant() -eq "msix") {
            try { $msixEnabled = [bool]$cap.msix.enabled } catch { }
            break
          }
          # Some QEMU builds may not provide `id` but still include a `msix` object.
          try {
            if ($cap.PSObject.Properties.Name -contains "msix") {
              try { $msixEnabled = [bool]$cap.msix.enabled } catch { }
              break
            }
          } catch { }
        }
      } catch { }

      $devBus = Convert-AeroPciInt $dev.bus
      if ($null -eq $devBus) { $devBus = $busNum }
      $slot = Convert-AeroPciInt $dev.slot
      $function = Convert-AeroPciInt $dev.function

      $infos += [pscustomobject]@{
        VendorId    = $vendor
        DeviceId    = $device
        Bus         = $devBus
        Slot        = $slot
        Function    = $function
        MsixEnabled = $msixEnabled
        Source      = "query-pci"
      }
    }
  }
  return $infos
}

function Get-AeroPciMsixInfoFromInfoPci {
  param(
    [Parameter(Mandatory = $true)] [string]$InfoPciText
  )

  $infos = @()
  $bus = $null
  $slot = $null
  $function = $null
  $vendor = $null
  $device = $null
  $msix = $null

  foreach ($raw in ($InfoPciText -split "`n")) {
    $line = $raw.TrimEnd("`r")
    if ($line -match "^Bus\s+(\d+),\s*device\s+(\d+),\s*function\s+(\d+):") {
      if (($null -ne $vendor) -and ($null -ne $device)) {
        $infos += [pscustomobject]@{
          VendorId    = $vendor
          DeviceId    = $device
          Bus         = $bus
          Slot        = $slot
          Function    = $function
          MsixEnabled = $msix
          Source      = "info pci"
        }
      }
      $bus = Convert-AeroPciInt $Matches[1]
      $slot = Convert-AeroPciInt $Matches[2]
      $function = Convert-AeroPciInt $Matches[3]
      $vendor = $null
      $device = $null
      $msix = $null
      continue
    }

    if ($line -match "\bVendor\s+ID:\s*([0-9a-fA-Fx]+)\s+Device\s+ID:\s*([0-9a-fA-Fx]+)\b") {
      $vendor = Convert-AeroPciInt $Matches[1]
      $device = Convert-AeroPciInt $Matches[2]
      continue
    }

    if (($null -eq $vendor) -or ($null -eq $device)) {
      if ($line -match "\b([0-9a-fA-F]{4}):([0-9a-fA-F]{4})\b") {
        $vendor = Convert-AeroPciInt ("0x" + $Matches[1])
        $device = Convert-AeroPciInt ("0x" + $Matches[2])
      }
    }

    if ($line -match "(?i)msi-x|msix") {
      $low = $line.ToLowerInvariant()
      if ($low.Contains("disabled") -or $low.Contains("enable-") -or $low.Contains("off")) {
        $msix = $false
      } elseif ($low.Contains("enabled") -or $low.Contains("enable+")) {
        $msix = $true
      }
    }
  }
  if (($null -ne $vendor) -and ($null -ne $device)) {
    $infos += [pscustomobject]@{
      VendorId    = $vendor
      DeviceId    = $device
      Bus         = $bus
      Slot        = $slot
      Function    = $function
      MsixEnabled = $msix
      Source      = "info pci"
    }
  }
  return $infos
}

function Test-AeroQmpRequiredVirtioMsix {
  param(
    [Parameter(Mandatory = $true)] [string]$Host,
    [Parameter(Mandatory = $true)] [int]$Port,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioNetMsix = $false,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioBlkMsix = $false,
    [Parameter(Mandatory = $false)] [bool]$RequireVirtioSndMsix = $false
  )

  if (-not ($RequireVirtioNetMsix -or $RequireVirtioBlkMsix -or $RequireVirtioSndMsix)) {
    return @{ Ok = $true }
  }

  $vendorId = 0x1AF4
  $reqs = @()
  if ($RequireVirtioNetMsix) { $reqs += @{ Name = "virtio-net"; Token = "VIRTIO_NET_MSIX_NOT_ENABLED"; DeviceIds = @(0x1041, 0x1000) } }
  if ($RequireVirtioBlkMsix) { $reqs += @{ Name = "virtio-blk"; Token = "VIRTIO_BLK_MSIX_NOT_ENABLED"; DeviceIds = @(0x1042, 0x1001) } }
  if ($RequireVirtioSndMsix) { $reqs += @{ Name = "virtio-snd"; Token = "VIRTIO_SND_MSIX_NOT_ENABLED"; DeviceIds = @(0x1059) } }

  $client = $null
  try {
    $client = [System.Net.Sockets.TcpClient]::new()
    $client.ReceiveTimeout = 2000
    $client.SendTimeout = 2000
    $client.Connect($Host, $Port)

    $stream = $client.GetStream()
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $false, 4096, $true)
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.Encoding]::UTF8, 4096, $true)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    # Greeting.
    $null = $reader.ReadLine()
    $null = Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "qmp_capabilities" }

    $queryInfos = @()
    $infoInfos = @()
    $querySupported = $false
    $infoSupported = $false

    try {
      $query = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "query-pci" }).return
      $queryInfos = Get-AeroPciMsixInfoFromQueryPci -QueryPciReturn $query
      $querySupported = $true
    } catch { }

    try {
      $txt = (Invoke-AeroQmpCommand -Writer $writer -Reader $reader -Command @{ execute = "human-monitor-command"; arguments = @{ "command-line" = "info pci" } }).return
      if ($null -ne $txt) {
        $infoInfos = Get-AeroPciMsixInfoFromInfoPci -InfoPciText ([string]$txt)
      }
      $infoSupported = $true
    } catch { }

    if ((-not $querySupported) -and (-not $infoSupported)) {
      return @{ Ok = $false; Result = "QMP_MSIX_CHECK_UNSUPPORTED"; Reason = "QEMU QMP does not support query-pci or human-monitor-command (required for MSI-X verification)" }
    }

    foreach ($r in $reqs) {
      $deviceName = [string]$r.Name
      $token = [string]$r.Token
      $deviceIds = [int[]]$r.DeviceIds

      $q = @($queryInfos | Where-Object { $_.VendorId -eq $vendorId -and ($deviceIds -contains $_.DeviceId) })
      $h = @($infoInfos | Where-Object { $_.VendorId -eq $vendorId -and ($deviceIds -contains $_.DeviceId) })
      $any = @($q + $h)

      if ($any.Count -eq 0) {
        $idsStr = ($deviceIds | ForEach-Object { "{0:x4}:{1:x4}" -f $vendorId, $_ }) -join ","
        return @{ Ok = $false; Result = $token; Reason = "did not find $deviceName PCI function(s) ($idsStr) in QEMU PCI introspection output" }
      }

      $matches = $null
      if (($q.Count -gt 0) -and (@($q | Where-Object { $null -eq $_.MsixEnabled }).Count -eq 0)) {
        $matches = $q
      } elseif (($h.Count -gt 0) -and (@($h | Where-Object { $null -eq $_.MsixEnabled }).Count -eq 0)) {
        $matches = $h
      }

      if ($null -eq $matches) {
        $idsStr = ($deviceIds | ForEach-Object { "{0:x4}:{1:x4}" -f $vendorId, $_ }) -join ","
        $bdf = Format-AeroPciBdf -Bus ($any[0].Bus) -Slot ($any[0].Slot) -Function ($any[0].Function)
        return @{ Ok = $false; Result = "QMP_MSIX_CHECK_UNSUPPORTED"; Reason = "could not determine MSI-X enabled state for $deviceName PCI function(s) ($idsStr) (example_bdf=$bdf)" }
      }

      $disabled = @($matches | Where-Object { -not $_.MsixEnabled })
      if ($disabled.Count -gt 0) {
        $d = $disabled[0]
        $bdf = Format-AeroPciBdf -Bus ($d.Bus) -Slot ($d.Slot) -Function ($d.Function)
        $idStr = ("{0:x4}:{1:x4}" -f $d.VendorId, $d.DeviceId)
        return @{ Ok = $false; Result = $token; Reason = "$deviceName PCI function $idStr at $bdf reported MSI-X disabled (source=$($d.Source))" }
      }
    }

    return @{ Ok = $true }
  } catch {
    $msg = ""
    try { $msg = [string]$_.Exception.Message } catch { }
    if ([string]::IsNullOrEmpty($msg)) { $msg = "unknown" }
    return @{ Ok = $false; Result = "QMP_MSIX_CHECK_FAILED"; Reason = "failed to query PCI MSI-X state via QMP: $msg" }
  } finally {
    if ($client) { $client.Close() }
  }
}

$DiskImagePath = (Resolve-Path -LiteralPath $DiskImagePath).Path

$serialParent = Split-Path -Parent $SerialLogPath
if ([string]::IsNullOrEmpty($serialParent)) { $serialParent = "." }
if (-not (Test-Path -LiteralPath $serialParent)) {
  New-Item -ItemType Directory -Path $serialParent -Force | Out-Null
}
$SerialLogPath = Join-Path (Resolve-Path -LiteralPath $serialParent).Path (Split-Path -Leaf $SerialLogPath)

if (Test-Path -LiteralPath $SerialLogPath) {
  Remove-Item -LiteralPath $SerialLogPath -Force
}

Write-Host "Starting HTTP server on 127.0.0.1:$HttpPort$HttpPath ..."
$httpLargePath = Get-AeroSelftestLargePath -Path $HttpPath
Write-Host "  (large payload at 127.0.0.1:$HttpPort$httpLargePath, 1 MiB deterministic bytes)"
Write-Host "  (guest: http://10.0.2.2:$HttpPort$HttpPath and http://10.0.2.2:$HttpPort$httpLargePath)"
$httpListener = Start-AeroSelftestHttpServer -Port $HttpPort -Path $HttpPath

$udpSocket = $null
if ($DisableUdp) {
  Write-Host "UDP echo server disabled (-DisableUdp)"
} else {
  Write-Host "Starting UDP echo server on 127.0.0.1:$UdpPort (guest: 10.0.2.2:$UdpPort) ..."
  try {
    $udpSocket = Start-AeroSelftestUdpEchoServer -Port $UdpPort
  } catch {
    throw "Failed to bind UDP echo server on 127.0.0.1:$UdpPort (port in use?): $_"
  }
}

try {
  $qmpPort = $null
  $qmpArgs = @()
  $needInputWheel = [bool]$WithInputWheel
  $needInputEventsExtended = [bool]$WithInputEventsExtended
  $needInputEvents = ([bool]$WithInputEvents) -or $needInputWheel -or $needInputEventsExtended
  $needInputMediaKeys = [bool]$WithInputMediaKeys
  $needInputTabletEvents = [bool]$WithInputTabletEvents
  $needVirtioTablet = [bool]$WithVirtioTablet -or $needInputTabletEvents
  $requestedVirtioNetVectors = $(if ($VirtioNetVectors -gt 0) { $VirtioNetVectors } else { $VirtioMsixVectors })
  $requestedVirtioBlkVectors = $(if ($VirtioBlkVectors -gt 0) { $VirtioBlkVectors } else { $VirtioMsixVectors })
  $requestedVirtioSndVectors = $(if ($VirtioSndVectors -gt 0) { $VirtioSndVectors } else { $VirtioMsixVectors })
  $requestedVirtioInputVectors = $(if ($VirtioInputVectors -gt 0) { $VirtioInputVectors } else { $VirtioMsixVectors })
  $virtioNetVectorsFlag = $(if ($VirtioNetVectors -gt 0) { "-VirtioNetVectors" } else { "-VirtioMsixVectors" })
  $virtioBlkVectorsFlag = $(if ($VirtioBlkVectors -gt 0) { "-VirtioBlkVectors" } else { "-VirtioMsixVectors" })
  $virtioSndVectorsFlag = $(if ($VirtioSndVectors -gt 0) { "-VirtioSndVectors" } else { "-VirtioMsixVectors" })
  $virtioInputVectorsFlag = $(if ($VirtioInputVectors -gt 0) { "-VirtioInputVectors" } else { "-VirtioMsixVectors" })
  $needMsixCheck = [bool]$RequireVirtioNetMsix -or [bool]$RequireVirtioBlkMsix -or [bool]$RequireVirtioSndMsix
  $needQmp = ($WithVirtioSnd -and $VirtioSndAudioBackend -eq "wav") -or $needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or $needMsixCheck -or [bool]$QemuPreflightPci
  if ($needQmp) {
    # QMP channel:
    # - Used for graceful shutdown when using the `wav` audiodev backend (so the RIFF header is finalized).
    # - Also used for virtio-input event injection (`input-send-event`) when -WithInputEvents/-WithInputMediaKeys is set.
    # - Also used for virtio PCI MSI-X enable verification (query-pci / info pci) when -RequireVirtio*Msix is set.
    # - Also used for the optional virtio PCI ID preflight (-QemuPreflightPci/-QmpPreflightPci).
    try {
      $qmpPort = Get-AeroFreeTcpPort
      $qmpArgs = @(
        "-qmp", "tcp:127.0.0.1:$qmpPort,server,nowait"
      )
    } catch {
      if ($needInputEvents -or $needInputMediaKeys -or $needInputTabletEvents -or [bool]$QemuPreflightPci) {
        throw "Failed to allocate QMP port required for input injection flags (-WithInputEvents/-WithVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra, -WithInputMediaKeys/-WithVirtioInputMediaKeys/-EnableVirtioInputMediaKeys, -WithInputTabletEvents/-WithTabletEvents) or -QemuPreflightPci/-QmpPreflightPci: $_"
      }
      Write-Warning "Failed to allocate QMP port for graceful shutdown: $_"
      $qmpPort = $null
      $qmpArgs = @()
    }
  }

  $serialChardev = "file,id=charserial0,path=$(Quote-AeroWin7QemuKeyvalValue $SerialLogPath)"
  $netdev = "user,id=net0"
  $serialBase = [System.IO.Path]::GetFileNameWithoutExtension((Split-Path -Leaf $SerialLogPath))
  $qemuStderrPath = Join-Path (Split-Path -Parent $SerialLogPath) "$serialBase.qemu.stderr.log"
  if (Test-Path -LiteralPath $qemuStderrPath) {
    Remove-Item -LiteralPath $qemuStderrPath -Force
  }
  if ($VirtioTransitional) {
    $netVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-net-pci" -Vectors $requestedVirtioNetVectors -ParamName $virtioNetVectorsFlag
    $nic = "virtio-net-pci,netdev=net0"
    if ($netVectors -gt 0) { $nic += ",vectors=$netVectors" }

    $virtioBlkArgs = @()
    $blkVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-blk-pci" -Vectors $requestedVirtioBlkVectors -ParamName $virtioBlkVectorsFlag
    if ($blkVectors -gt 0) {
      # Use an explicit virtio-blk-pci device so we can apply `vectors=`.
      $driveId = "drive0"
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=none,id=$driveId,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $blk = "virtio-blk-pci,drive=$driveId,vectors=$blkVectors"
      $virtioBlkArgs = @(
        "-drive", $drive,
        "-device", $blk
      )
    } else {
      $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=virtio,cache=writeback"
      if ($Snapshot) { $drive += ",snapshot=on" }
      $virtioBlkArgs = @(
        "-drive", $drive
      )
    }

    # Transitional mode is primarily an escape hatch for older QEMU builds (or intentionally
    # testing legacy driver packages). The guest selftest still expects virtio-input devices,
    # so attach virtio-keyboard-pci + virtio-mouse-pci when the QEMU binary supports them.
    $virtioInputArgs = @()
    $haveVirtioKbd = $false
    $haveVirtioMouse = $false
    $haveVirtioTablet = $false
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci"
      $haveVirtioKbd = $true
    } catch { }
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci"
      $haveVirtioMouse = $true
    } catch { }
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci"
      $haveVirtioTablet = $true
    } catch { }

    if ($needInputEvents -and (-not ($haveVirtioKbd -and $haveVirtioMouse))) {
      throw "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra). Upgrade QEMU or omit input event injection."
    }
    if ($needInputMediaKeys -and (-not $haveVirtioKbd)) {
      throw "QEMU does not advertise virtio-keyboard-pci but -WithInputMediaKeys was enabled. Upgrade QEMU or omit media key injection."
    }
    if (-not ($haveVirtioKbd -and $haveVirtioMouse)) {
      Write-Warning "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci. The guest virtio-input selftest will likely FAIL. Upgrade QEMU or adjust the guest image/selftest expectations."
    }

    if ($haveVirtioKbd) {
      $kbdArg = "virtio-keyboard-pci,id=$($script:VirtioInputKeyboardQmpId)"
      $kbdVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
      if ($kbdVectors -gt 0) { $kbdArg += ",vectors=$kbdVectors" }
      $virtioInputArgs += @(
        "-device", $kbdArg
      )
    }
    if ($haveVirtioMouse) {
      $mouseArg = "virtio-mouse-pci,id=$($script:VirtioInputMouseQmpId)"
      $mouseVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
      if ($mouseVectors -gt 0) { $mouseArg += ",vectors=$mouseVectors" }
      $virtioInputArgs += @(
        "-device", $mouseArg
      )
    }
    if ($needVirtioTablet) {
      if (-not $haveVirtioTablet) {
        throw "QEMU does not advertise virtio-tablet-pci but -WithVirtioTablet/-WithInputTabletEvents/-WithTabletEvents was enabled. Upgrade QEMU or omit tablet support."
      }
      $tabletArg = "virtio-tablet-pci,id=$($script:VirtioInputTabletQmpId)"
      $tabletVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
      if ($tabletVectors -gt 0) { $tabletArg += ",vectors=$tabletVectors" }
      $virtioInputArgs += @(
        "-device", $tabletArg
      )
    }
    $attachedVirtioInput = ($virtioInputArgs.Count -gt 0)

    $virtioSndArgs = @()
    if ($WithVirtioSnd) {
      $audiodev = ""
      switch ($VirtioSndAudioBackend) {
        "none" {
          $audiodev = "none,id=snd0"
        }
        "wav" {
          if ([string]::IsNullOrEmpty($VirtioSndWavPath)) {
            throw "VirtioSndWavPath is required when VirtioSndAudioBackend is 'wav'."
          }

          $wavParent = Split-Path -Parent $VirtioSndWavPath
          if ([string]::IsNullOrEmpty($wavParent)) { $wavParent = "." }
          if (-not (Test-Path -LiteralPath $wavParent)) {
            New-Item -ItemType Directory -Path $wavParent -Force | Out-Null
          }
          $VirtioSndWavPath = Join-Path (Resolve-Path -LiteralPath $wavParent).Path (Split-Path -Leaf $VirtioSndWavPath)

          if (Test-Path -LiteralPath $VirtioSndWavPath) {
            Remove-Item -LiteralPath $VirtioSndWavPath -Force
          }

          $audiodev = "wav,id=snd0,path=$(Quote-AeroWin7QemuKeyvalValue $VirtioSndWavPath)"
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $false -MsixVectors $requestedVirtioSndVectors -VectorsParamName $virtioSndVectorsFlag -DeviceName $virtioSndPciDeviceName
      $virtioSndArgs = @(
        "-audiodev", $audiodev,
        "-device", $virtioSndDevice
      )
    } elseif (-not [string]::IsNullOrEmpty($VirtioSndWavPath) -or $VirtioSndAudioBackend -ne "none") {
      throw "-VirtioSndAudioBackend/-VirtioSndWavPath require -WithVirtioSnd."
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic
    ) + $virtioInputArgs + $virtioBlkArgs + $virtioSndArgs + $QemuExtraArgs
  } else {
    # Ensure the QEMU binary supports the modern-only + contract revision properties we rely on.
    Assert-AeroWin7QemuSupportsAeroW7VirtioContractV1 -QemuSystem $QemuSystem -WithVirtioInput -WithVirtioTablet:$needVirtioTablet
    # Force modern-only virtio-pci IDs (DEV_1041/DEV_1042/DEV_1052) per AERO-W7-VIRTIO v1.
    # The shared QEMU arg helpers also set PCI Revision ID = 0x01 so strict contract-v1
    # drivers bind under QEMU.
    $netVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-net-pci" -Vectors $requestedVirtioNetVectors -ParamName $virtioNetVectorsFlag
    $blkVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-blk-pci" -Vectors $requestedVirtioBlkVectors -ParamName $virtioBlkVectorsFlag
    $kbdVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
    $mouseVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
    $tabletVectors = 0
    if ($needVirtioTablet) {
      $tabletVectors = Resolve-AeroWin7QemuMsixVectors -QemuSystem $QemuSystem -DeviceName "virtio-tablet-pci" -Vectors $requestedVirtioInputVectors -ParamName $virtioInputVectorsFlag
    }

    $nic = New-AeroWin7VirtioNetDeviceArg -NetdevId "net0" -MsixVectors $netVectors
    $driveId = "drive0"
    $drive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $driveId -Snapshot:$Snapshot
    $blk = New-AeroWin7VirtioBlkDeviceArg -DriveId $driveId -MsixVectors $blkVectors

    $kbd = "$(New-AeroWin7VirtioKeyboardDeviceArg -MsixVectors $kbdVectors),id=$($script:VirtioInputKeyboardQmpId)"
    $mouse = "$(New-AeroWin7VirtioMouseDeviceArg -MsixVectors $mouseVectors),id=$($script:VirtioInputMouseQmpId)"
    $attachedVirtioInput = $true
    $virtioTabletArgs = @()
    if ($needVirtioTablet) {
      $tablet = "$(New-AeroWin7VirtioTabletDeviceArg -MsixVectors $tabletVectors),id=$($script:VirtioInputTabletQmpId)"
      $virtioTabletArgs = @(
        "-device", $tablet
      )
    }

    $virtioSndArgs = @()
    if ($WithVirtioSnd) {
      $audiodev = ""
      switch ($VirtioSndAudioBackend) {
        "none" {
          $audiodev = "none,id=snd0"
        }
        "wav" {
          if ([string]::IsNullOrEmpty($VirtioSndWavPath)) {
            throw "VirtioSndWavPath is required when VirtioSndAudioBackend is 'wav'."
          }

          $wavParent = Split-Path -Parent $VirtioSndWavPath
          if ([string]::IsNullOrEmpty($wavParent)) { $wavParent = "." }
          if (-not (Test-Path -LiteralPath $wavParent)) {
            New-Item -ItemType Directory -Path $wavParent -Force | Out-Null
          }
          $VirtioSndWavPath = Join-Path (Resolve-Path -LiteralPath $wavParent).Path (Split-Path -Leaf $VirtioSndWavPath)

          if (Test-Path -LiteralPath $VirtioSndWavPath) {
            Remove-Item -LiteralPath $VirtioSndWavPath -Force
          }

          $audiodev = "wav,id=snd0,path=$(Quote-AeroWin7QemuKeyvalValue $VirtioSndWavPath)"
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $true -MsixVectors $requestedVirtioSndVectors -VectorsParamName $virtioSndVectorsFlag -DeviceName $virtioSndPciDeviceName
      $virtioSndArgs = @(
        "-audiodev", $audiodev,
        "-device", $virtioSndDevice
      )
    } elseif (-not [string]::IsNullOrEmpty($VirtioSndWavPath) -or $VirtioSndAudioBackend -ne "none") {
      throw "-VirtioSndAudioBackend/-VirtioSndWavPath require -WithVirtioSnd."
    }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot"
    ) + $qmpArgs + @(
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic,
      "-device", $kbd,
      "-device", $mouse
    ) + $virtioTabletArgs + @(
      "-drive", $drive,
      "-device", $blk
    ) + $virtioSndArgs + $QemuExtraArgs
  }

  Write-Host "Launching QEMU:"
  Write-Host "  $QemuSystem $($qemuArgs -join ' ')"
 
  $proc = Start-Process -FilePath $QemuSystem -ArgumentList $qemuArgs -PassThru -RedirectStandardError $qemuStderrPath
  $scriptExitCode = 0
  
  $result = $null
  try {
    if ([bool]$QemuPreflightPci) {
      if (($null -eq $qmpPort) -or ($qmpPort -le 0)) {
        $result = @{
          Result = "QEMU_PCI_PREFLIGHT_FAILED"
          Tail   = ""
          Reason = "QMP endpoint not configured"
        }
      } else {
        $pref = Test-AeroQmpVirtioPciPreflight `
          -Host "127.0.0.1" `
          -Port ([int]$qmpPort) `
          -VirtioTransitional ([bool]$VirtioTransitional) `
          -WithVirtioSnd ([bool]$WithVirtioSnd) `
          -WithVirtioTablet ([bool]$needVirtioTablet)
        if (-not $pref.Ok) {
          $result = @{
            Result = "QEMU_PCI_PREFLIGHT_FAILED"
            Tail   = ""
            Reason = $pref.Reason
          }
        }
      }
    }

    if ($null -eq $result) {
      $result = Wait-AeroSelftestResult `
        -SerialLogPath $SerialLogPath `
        -QemuProcess $proc `
        -TimeoutSeconds $TimeoutSeconds `
        -HttpListener $httpListener `
        -HttpPath $HttpPath `
        -UdpSocket $udpSocket `
        -FollowSerial ([bool]$FollowSerial) `
        -RequirePerTestMarkers (-not $VirtioTransitional) `
        -RequireVirtioNetUdpPass (-not $DisableUdp) `
        -RequireVirtioSndPass ([bool]$WithVirtioSnd) `
        -RequireVirtioSndBufferLimitsPass ([bool]$WithSndBufferLimits) `
        -RequireVirtioInputEventsPass ([bool]$needInputEvents) `
        -RequireVirtioInputMediaKeysPass ([bool]$needInputMediaKeys) `
        -RequireVirtioInputWheelPass ([bool]$needInputWheel) `
        -RequireVirtioInputEventsExtendedPass ([bool]$needInputEventsExtended) `
        -RequireVirtioInputMsixPass ([bool]$RequireVirtioInputMsix) `
        -RequireVirtioInputTabletEventsPass ([bool]$needInputTabletEvents) `
        -RequireExpectBlkMsi ([bool]$RequireExpectBlkMsi) `
        -QmpHost "127.0.0.1" `
        -QmpPort $qmpPort
    }
    if ($RequireVirtioBlkMsix -and $result.Result -eq "PASS") {
      # In addition to the host-side PCI MSI-X enable check (QMP), require the guest to report
      # virtio-blk running in MSI-X mode via the dedicated marker:
      #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...
      $chk = Test-AeroVirtioBlkMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
      if (-not $chk.Ok) {
        $result = @{
          Result     = "VIRTIO_BLK_MSIX_REQUIRED"
          Tail       = $result.Tail
          MsixReason = $chk.Reason
        }
      }
    }
    if ($RequireVirtioSndMsix -and $result.Result -eq "PASS") {
      # In addition to the host-side PCI MSI-X enable check (QMP), require the guest to report
      # virtio-snd running in MSI-X mode via the dedicated marker:
      #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...
      $chk = Test-AeroVirtioSndMsixMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
      if (-not $chk.Ok) {
        $result = @{
          Result     = "VIRTIO_SND_MSIX_REQUIRED"
          Tail       = $result.Tail
          MsixReason = $chk.Reason
        }
      }
    }
    if ($needMsixCheck -and $result.Result -eq "PASS") {
      if (($null -eq $qmpPort) -or ($qmpPort -le 0)) {
        $result = @{
          Result     = "QMP_MSIX_CHECK_UNSUPPORTED"
          Tail       = $result.Tail
          MsixReason = "QMP endpoint not configured"
        }
      } else {
        $msix = Test-AeroQmpRequiredVirtioMsix -Host "127.0.0.1" -Port ([int]$qmpPort) -RequireVirtioNetMsix ([bool]$RequireVirtioNetMsix) -RequireVirtioBlkMsix ([bool]$RequireVirtioBlkMsix) -RequireVirtioSndMsix ([bool]$RequireVirtioSndMsix)
        if (-not $msix.Ok) {
          $result = @{
            Result     = $msix.Result
            Tail       = $result.Tail
            MsixReason = $msix.Reason
          }
        }
      }
    }
  } finally {
    if (-not $proc.HasExited) {
      $quitOk = $false
      if ($null -ne $qmpPort) {
        $quitOk = Try-AeroQmpQuit -Host "127.0.0.1" -Port $qmpPort
      }
      if ($quitOk) {
        try { $proc.WaitForExit(10000) } catch { }
      }

      # Only fall back to hard termination if QEMU did not exit after the QMP quit request.
      if (-not $proc.HasExited) {
        Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
        try { $proc.WaitForExit(5000) } catch { }
        if (-not $proc.HasExited) {
          Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
          try { $proc.WaitForExit(5000) } catch { }
        }
      }
    }
  }

  Try-EmitAeroVirtioBlkIrqMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioBlkIoMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioNetLargeMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-net" -HostMarker "VIRTIO_NET_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-snd" -HostMarker "VIRTIO_SND_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqMarkerFromTestMarker -Tail $result.Tail -Device "virtio-input" -HostMarker "VIRTIO_INPUT_IRQ" -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndCaptureMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndEventqMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndFormatMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndDuplexMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioSndBufferLimitsMarker -Tail $result.Tail -SerialLogPath $SerialLogPath
  Try-EmitAeroVirtioIrqDiagnosticsMarkers -Tail $result.Tail -SerialLogPath $SerialLogPath

  switch ($result.Result) {
    "PASS" {
      if ($RequireIntx -or $RequireMsi) {
        $expected = if ($RequireIntx) { "intx" } else { "msi" }
        $devices = @("virtio-blk", "virtio-net")
        if ($attachedVirtioInput) { $devices += "virtio-input" }
        if ($WithVirtioSnd) { $devices += "virtio-snd" }
        $chk = Test-AeroVirtioIrqModeEnforcement -Tail $result.Tail -Devices $devices -Expected $expected
        if (-not $chk.Ok) {
          Write-Host "FAIL: IRQ_MODE_MISMATCH: $($chk.Device) expected=$($chk.Expected) got=$($chk.Got)"
          if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
            Write-Host "`n--- Serial tail ---"
            Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
          }
          $scriptExitCode = 1
          break
        }
      }

      Write-Host "PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS"
      $scriptExitCode = 0
    }
    "FAIL" {
      Write-Host "FAIL: SELFTEST_FAILED: AERO_VIRTIO_SELFTEST|RESULT|FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QEMU_EXITED" {
      $exitCode = $null
      try { $exitCode = $proc.ExitCode } catch { }
      Write-Host "FAIL: QEMU_EXITED: QEMU exited before selftest result marker (exit code: $exitCode)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      Write-AeroQemuStderrTail -Path $qemuStderrPath
      $scriptExitCode = 3
    }
    "TIMEOUT" {
      Write-Host "FAIL: TIMEOUT: timed out waiting for AERO_VIRTIO_SELFTEST result marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 2
    }
    "EXPECT_BLK_MSI_NOT_SET" {
      Write-Host "FAIL: EXPECT_BLK_MSI_NOT_SET: guest selftest was not provisioned with --expect-blk-msi (expect_blk_msi=1 in CONFIG marker). Re-provision with New-AeroWin7TestImage.ps1 -ExpectBlkMsi or set AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1."
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_BLK" {
      Write-Host "FAIL: MISSING_VIRTIO_BLK: selftest RESULT=PASS but did not emit virtio-blk test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_FAILED" {
      Write-Host "FAIL: VIRTIO_BLK_FAILED: selftest RESULT=PASS but virtio-blk test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT: selftest RESULT=PASS but did not emit virtio-input test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_FAILED: selftest RESULT=PASS but virtio-input test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_MSIX" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while -RequireVirtioInputMsix was enabled (guest selftest too old?)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MSIX_REQUIRED" {
      Write-Host "FAIL: VIRTIO_INPUT_MSIX_REQUIRED: virtio-input-msix marker did not report mode=msix while -RequireVirtioInputMsix was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_EVENTS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events marker (READY/SKIP/PASS/FAIL) after virtio-input completed while input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra) (guest selftest too old or missing --test-input-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_EVENTS_EXTENDED" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe virtio-input-events-modifiers/buttons/wheel markers while -WithInputEventsExtended/-WithInputEventsExtra was enabled (guest selftest too old or missing --test-input-events-extended)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra) (provision the guest with --test-input-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: virtio-input-events-* extended tests were skipped (flag_not_set) but -WithInputEventsExtended/-WithInputEventsExtra was enabled (provision the guest with --test-input-events-extended)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while input injection flags were enabled (-WithInputEvents/-WithVirtioInputEvents/-EnableVirtioInputEvents, -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel, -WithInputEventsExtended/-WithInputEventsExtra)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_MEDIA_KEYS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys marker (READY/SKIP/PASS/FAIL) after virtio-input completed while -WithInputMediaKeys was enabled (guest selftest too old or missing --test-input-media-keys)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MEDIA_KEYS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but -WithInputMediaKeys was enabled (provision the guest with --test-input-media-keys)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_MEDIA_KEYS_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while -WithInputMediaKeys was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MEDIA_KEYS_UNSUPPORTED" {
      Write-Host "FAIL: QMP_MEDIA_KEYS_UNSUPPORTED: failed to inject virtio-input media keys via QMP (ensure QMP is reachable and QEMU supports input-send-event + multimedia qcodes)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QEMU_PCI_PREFLIGHT_FAILED" {
      $reason = ""
      try { $reason = [string]$result.Reason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "QMP query-pci preflight failed" }
      Write-Host "FAIL: QEMU_PCI_PREFLIGHT_FAILED: $reason"
      Write-AeroQemuStderrTail -Path $qemuStderrPath
      $scriptExitCode = 2
    }
    "MISSING_VIRTIO_INPUT_WHEEL" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel marker while -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel was enabled (guest selftest too old or missing wheel coverage)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_WHEEL_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_WHEEL_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while -WithInputWheel/-WithVirtioInputWheel/-EnableVirtioInputWheel was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_EXTENDED_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: one or more virtio-input-events-* extended tests reported FAIL while -WithInputEventsExtended/-WithInputEventsExtra was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_INPUT_INJECT_FAILED" {
      Write-Host "FAIL: QMP_INPUT_INJECT_FAILED: failed to inject virtio-input events via QMP (ensure QMP is reachable and QEMU supports input-send-event)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_TABLET_EVENTS" {
      Write-Host "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events marker (READY/SKIP/PASS/FAIL) after virtio-input completed while -WithInputTabletEvents/-WithTabletEvents was enabled (guest selftest too old or missing --test-input-tablet-events/--test-tablet-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_TABLET_EVENTS_SKIPPED" {
      Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but -WithInputTabletEvents/-WithTabletEvents was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_TABLET_EVENTS_FAILED" {
      Write-Host "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while -WithInputTabletEvents/-WithTabletEvents was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_INPUT_TABLET_INJECT_FAILED" {
      Write-Host "FAIL: QMP_INPUT_TABLET_INJECT_FAILED: failed to inject virtio-input tablet events via QMP (ensure QMP is reachable and QEMU supports input-send-event)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_FAILED" {
      Write-Host "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_CAPTURE_FAILED" {
      Write-Host "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_DUPLEX_FAILED" {
      Write-Host "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND" {
      Write-Host "FAIL: MISSING_VIRTIO_SND: selftest RESULT=PASS but did not emit virtio-snd test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_CAPTURE" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_CAPTURE: selftest RESULT=PASS but did not emit virtio-snd-capture test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_DUPLEX" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_BUFFER_LIMITS_FAILED" {
      Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED: selftest RESULT=PASS but virtio-snd-buffer-limits test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_BUFFER_LIMITS" {
      Write-Host "FAIL: MISSING_VIRTIO_SND_BUFFER_LIMITS: selftest RESULT=PASS but did not emit virtio-snd-buffer-limits test marker (provision the guest with --test-snd-buffer-limits)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET" {
      Write-Host "FAIL: MISSING_VIRTIO_NET: selftest RESULT=PASS but did not emit virtio-net test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET_UDP" {
      Write-Host "FAIL: MISSING_VIRTIO_NET_UDP: selftest RESULT=PASS but did not emit virtio-net-udp test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_FAILED" {
      Write-Host "FAIL: VIRTIO_NET_FAILED: selftest RESULT=PASS but virtio-net test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_FAILED" {
      Write-Host "FAIL: VIRTIO_NET_UDP_FAILED: selftest RESULT=PASS but virtio-net-udp test reported FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_UDP_SKIPPED" {
      Write-Host "FAIL: VIRTIO_NET_UDP_SKIPPED: virtio-net-udp test was skipped but UDP testing is enabled (update/provision the guest selftest)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_NET_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-net MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_NET_MSIX_NOT_ENABLED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-blk MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_BLK_MSIX_NOT_ENABLED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_BLK_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "guest did not report virtio-blk running in MSI-X mode" }
      Write-Host "FAIL: VIRTIO_BLK_MSIX_REQUIRED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_MSIX_NOT_ENABLED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "virtio-snd MSI-X was not enabled" }
      Write-Host "FAIL: VIRTIO_SND_MSIX_NOT_ENABLED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_MSIX_REQUIRED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "guest did not report virtio-snd running in MSI-X mode" }
      Write-Host "FAIL: VIRTIO_SND_MSIX_REQUIRED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MSIX_CHECK_UNSUPPORTED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "QEMU QMP did not expose query-pci or human-monitor-command info pci" }
      Write-Host "FAIL: QMP_MSIX_CHECK_UNSUPPORTED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_MSIX_CHECK_FAILED" {
      $reason = ""
      try { $reason = [string]$result.MsixReason } catch { }
      if ([string]::IsNullOrEmpty($reason)) { $reason = "failed to query QEMU PCI MSI-X state via QMP" }
      Write-Host "FAIL: QMP_MSIX_CHECK_FAILED: $reason"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_SND_SKIPPED" {
      $reason = "unknown"
      if ($result.Tail -match "virtio-snd: skipped \\(enable with --test-snd\\)") {
        $reason = "guest_not_configured_with_--test-snd"
      } elseif ($result.Tail -match "virtio-snd: .*device not detected") {
        $reason = "device_missing"
      } elseif ($result.Tail -match "virtio-snd: disabled by --disable-snd") {
        $reason = "--disable-snd"
      }

      Write-Host "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped ($reason) but -WithVirtioSnd was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_CAPTURE_SKIPPED" {
      $reason = "unknown"
      if ($result.Tail -match "AERO_VIRTIO_SELFTEST\\|TEST\\|virtio-snd-capture\\|SKIP\\|([^\\|\\r\\n]+)") {
        $reason = $Matches[1]
      }

      Write-Host "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped ($reason) but -WithVirtioSnd was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_DUPLEX_SKIPPED" {
      $reason = "unknown"
      if ($result.Tail -match "AERO_VIRTIO_SELFTEST\\|TEST\\|virtio-snd-duplex\\|SKIP\\|([^\\|\\r\\n]+)") {
        $reason = $Matches[1]
      }

      Write-Host "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped ($reason) but -WithVirtioSnd was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "VIRTIO_SND_BUFFER_LIMITS_SKIPPED" {
      $reason = "unknown"
      if ($result.Tail -match "AERO_VIRTIO_SELFTEST\\|TEST\\|virtio-snd-buffer-limits\\|SKIP\\|([^\\|\\r\\n]+)") {
        $reason = $Matches[1]
      }

      if ($reason -eq "flag_not_set") {
        Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped (flag_not_set) but -WithSndBufferLimits was enabled (provision the guest with --test-snd-buffer-limits)"
      } else {
        Write-Host "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped ($reason) but -WithSndBufferLimits was enabled"
      }
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    default {
      Write-Host "FAIL: unexpected harness result: $($result.Result)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 4
    }
  }

  if ($VerifyVirtioSndWav) {
    $wavOk = Invoke-AeroVirtioSndWavVerification -WavPath $VirtioSndWavPath -PeakThreshold $VirtioSndWavPeakThreshold -RmsThreshold $VirtioSndWavRmsThreshold
    if (-not $wavOk -and $scriptExitCode -eq 0) {
      $scriptExitCode = 5
    }
  }

} finally {
  if ($httpListener) {
    try { $httpListener.Stop() } catch { }
  }
  if ($udpSocket) {
    try { $udpSocket.Close() } catch { }
    try { $udpSocket.Dispose() } catch { }
  }
}

exit $scriptExitCode
