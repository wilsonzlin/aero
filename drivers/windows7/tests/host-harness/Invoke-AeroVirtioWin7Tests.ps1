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

  # If set, inject deterministic keyboard/mouse events via QMP (`input-send-event`) and require the guest
  # virtio-input end-to-end event delivery marker (`virtio-input-events`) to PASS.
  #
  # Note: The guest image must be provisioned with `--test-input-events` (or env var equivalent) so the
  # guest selftest runs the read-report loop.
  [Parameter(Mandatory = $false)]
  [Alias("WithVirtioInputEvents", "EnableVirtioInputEvents")]
  [switch]$WithInputEvents,

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

  # If set, attach a virtio-snd device (virtio-sound-pci / virtio-snd-pci).
  # Note: the guest selftest always emits virtio-snd markers (playback + capture + duplex), but will report SKIP if the
  # virtio-snd PCI device is missing or the test was disabled. When -WithVirtioSnd is enabled, the harness
  # requires virtio-snd, virtio-snd-capture, and virtio-snd-duplex to PASS.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioSnd")]
  [switch]$WithVirtioSnd,

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
if ($VerifyVirtioSndWav) {
  if (-not $WithVirtioSnd) {
    throw "-VerifyVirtioSndWav requires -WithVirtioSnd."
  }
  if ($VirtioSndAudioBackend -ne "wav") {
    throw "-VerifyVirtioSndWav requires -VirtioSndAudioBackend wav."
  }
}

if ($VirtioTransitional -and $WithVirtioSnd) {
  throw "-VirtioTransitional is incompatible with -WithVirtioSnd (virtio-snd testing requires modern-only virtio-pci + contract revision overrides)."
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
      $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::ASCII, $false, 4096, $true)
      $requestLine = $reader.ReadLine()
      if ($null -eq $requestLine) { return $true }

      # Drain headers.
      while ($true) {
        $line = $reader.ReadLine()
        if ($null -eq $line -or $line.Length -eq 0) { break }
      }

      $ok = $false
      $large = $false
      if ($requestLine -match "^(GET|HEAD)\s+(\S+)\s+HTTP/") {
        $reqPath = $Matches[2]
        if ($reqPath -eq $Path) { $ok = $true }
        elseif ($reqPath -eq "$Path-large" -or $reqPath -eq (Get-AeroSelftestLargePath -Path $Path)) {
          $ok = $true
          $large = $true
        }
      }

      $statusLine = if ($ok) { "HTTP/1.1 200 OK" } else { "HTTP/1.1 404 Not Found" }
      $contentType = "text/plain"
      $bodyBytes = $null
      $etagHeader = $null
      if ($ok -and $large) {
        # Deterministic 1 MiB payload (0..255 repeating) for sustained virtio-net TX/RX stress.
        $contentType = "application/octet-stream"
        $etagHeader = "ETag: `"8505ae4435522325`""
        $bodyBytes = Get-AeroSelftestLargePayload
      } else {
        $body = if ($ok) { "OK`n" } else { "NOT_FOUND`n" }
        $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes($body)
      }

      $hdrLines = @(
        $statusLine,
        "Content-Type: $contentType",
        "Content-Length: $($bodyBytes.Length)",
        "Cache-Control: no-store",
        $etagHeader,
        "Connection: close",
        "",
        ""
      ) | Where-Object { $null -ne $_ -and $_.Length -gt 0 }
      $hdr = ($hdrLines + @("", "")) -join "`r`n"

      $hdrBytes = [System.Text.Encoding]::ASCII.GetBytes($hdr)
      $stream.Write($hdrBytes, 0, $hdrBytes.Length)
      if (-not ($requestLine -like "HEAD *")) {
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

function Resolve-AeroVirtioSndPciDeviceName {
  param(
    [Parameter(Mandatory = $true)] [string]$QemuSystem
  )

  # QEMU device naming has changed over time. Prefer the modern name but fall back
  # if a distro build exposes a legacy alias.
  $helpText = ""
  try {
    $helpText = (& $QemuSystem -device help 2>&1 | Out-String)
  } catch {
    throw "Failed to query QEMU device list ('$QemuSystem -device help'): $_"
  }

  if ($helpText -match "virtio-sound-pci") { return "virtio-sound-pci" }
  if ($helpText -match "virtio-snd-pci") { return "virtio-snd-pci" }

  throw "QEMU binary '$QemuSystem' does not advertise a virtio-snd PCI device. Upgrade QEMU or pass a custom device via -QemuExtraArgs."
}

function Wait-AeroSelftestResult {
  param(
    [Parameter(Mandatory = $true)] [string]$SerialLogPath,
    [Parameter(Mandatory = $true)] [System.Diagnostics.Process]$QemuProcess,
    [Parameter(Mandatory = $true)] [int]$TimeoutSeconds,
    [Parameter(Mandatory = $true)] $HttpListener,
    [Parameter(Mandatory = $true)] [string]$HttpPath,
    [Parameter(Mandatory = $true)] [bool]$FollowSerial,
    # When $true, require per-test markers so older selftest binaries cannot accidentally pass.
    [Parameter(Mandatory = $false)] [bool]$RequirePerTestMarkers = $true,
    # If true, a virtio-snd device was attached, so the virtio-snd selftest must actually run and pass
    # (not be skipped via --disable-snd).
    [Parameter(Mandatory = $true)] [bool]$RequireVirtioSndPass,
    # If true, require the optional virtio-input-events marker to PASS (host will inject events via QMP).
    [Parameter(Mandatory = $false)]
    [Alias("EnableVirtioInputEvents")]
    [bool]$RequireVirtioInputEventsPass = $false,
    # Best-effort QMP channel for input injection.
    [Parameter(Mandatory = $false)] [string]$QmpHost = "127.0.0.1",
    [Parameter(Mandatory = $false)] [Nullable[int]]$QmpPort = $null
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $pos = 0L
  $tail = ""
  $sawVirtioBlkPass = $false
  $sawVirtioBlkFail = $false
  $sawVirtioInputPass = $false
  $sawVirtioInputFail = $false
  $sawVirtioInputEventsReady = $false
  $sawVirtioInputEventsPass = $false
  $sawVirtioInputEventsFail = $false
  $sawVirtioInputEventsSkip = $false
  $injectedVirtioInputEvents = $false
  $sawVirtioSndPass = $false
  $sawVirtioSndSkip = $false
  $sawVirtioSndFail = $false
  $sawVirtioSndCapturePass = $false
  $sawVirtioSndCaptureSkip = $false
  $sawVirtioSndCaptureFail = $false
  $sawVirtioSndDuplexPass = $false
  $sawVirtioSndDuplexSkip = $false
  $sawVirtioSndDuplexFail = $false
  $sawVirtioNetPass = $false
  $sawVirtioNetFail = $false

  while ((Get-Date) -lt $deadline) {
    $null = Try-HandleAeroHttpRequest -Listener $HttpListener -Path $HttpPath

    $chunk = Read-NewText -Path $SerialLogPath -Position ([ref]$pos)
    if ($chunk.Length -gt 0) {
      if ($FollowSerial) {
        [Console]::Out.Write($chunk)
      }

      $tail += $chunk
      if ($tail.Length -gt 131072) { $tail = $tail.Substring($tail.Length - 131072) }

      if (-not $sawVirtioBlkPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|PASS") {
        $sawVirtioBlkPass = $true
      }
      if (-not $sawVirtioBlkFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|FAIL") {
        $sawVirtioBlkFail = $true
      }
      if (-not $sawVirtioInputPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|PASS") {
        $sawVirtioInputPass = $true
      }
      if (-not $sawVirtioInputFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|FAIL") {
        $sawVirtioInputFail = $true
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

      if ($RequireVirtioInputEventsPass -and $sawVirtioInputEventsReady -and (-not $injectedVirtioInputEvents)) {
        $injectedVirtioInputEvents = $true
        if (($null -eq $QmpPort) -or ($QmpPort -le 0)) {
          return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
        }
        $ok = Try-AeroQmpInjectVirtioInputEvents -Host $QmpHost -Port ([int]$QmpPort)
        if (-not $ok) {
          return @{ Result = "QMP_INPUT_INJECT_FAILED"; Tail = $tail }
        }
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
      if (-not $sawVirtioNetPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|PASS") {
        $sawVirtioNetPass = $true
      }
      if (-not $sawVirtioNetFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-net\|FAIL") {
        $sawVirtioNetFail = $true
      }

      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|PASS") {
        if ($RequirePerTestMarkers) {
          # Require per-test markers so older selftest binaries cannot accidentally pass the host harness.
          if ($sawVirtioBlkFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not $sawVirtioBlkPass) {
            return @{ Result = "MISSING_VIRTIO_BLK"; Tail = $tail }
          }
          if ($sawVirtioInputFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not $sawVirtioInputPass) {
            return @{ Result = "MISSING_VIRTIO_INPUT"; Tail = $tail }
          }

          # Also ensure the virtio-snd markers are present (playback + capture), so older selftest binaries
          # that predate virtio-snd testing cannot accidentally pass.
          if ($sawVirtioSndFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not ($sawVirtioSndPass -or $sawVirtioSndSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndPass)) {
            return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndCaptureFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not ($sawVirtioSndCapturePass -or $sawVirtioSndCaptureSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_CAPTURE"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndCapturePass)) {
            return @{ Result = "VIRTIO_SND_CAPTURE_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioSndDuplexFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not ($sawVirtioSndDuplexPass -or $sawVirtioSndDuplexSkip)) {
            return @{ Result = "MISSING_VIRTIO_SND_DUPLEX"; Tail = $tail }
          }
          if ($RequireVirtioSndPass -and (-not $sawVirtioSndDuplexPass)) {
            return @{ Result = "VIRTIO_SND_DUPLEX_SKIPPED"; Tail = $tail }
          }

          if ($sawVirtioNetFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not $sawVirtioNetPass) {
            return @{ Result = "MISSING_VIRTIO_NET"; Tail = $tail }
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
          }

          return @{ Result = "PASS"; Tail = $tail }
        }

        if ($RequireVirtioSndPass) {
          if ($sawVirtioSndFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if ($sawVirtioSndPass) {
            if ($sawVirtioSndCaptureFail -or $sawVirtioSndDuplexFail) {
              return @{ Result = "FAIL"; Tail = $tail }
            }
            if ($sawVirtioSndCapturePass) {
              if ($sawVirtioSndDuplexPass) {
                if ($RequireVirtioInputEventsPass) {
                  if ($sawVirtioInputEventsFail) { return @{ Result = "VIRTIO_INPUT_EVENTS_FAILED"; Tail = $tail } }
                  if (-not $sawVirtioInputEventsPass) {
                    if ($sawVirtioInputEventsSkip) { return @{ Result = "VIRTIO_INPUT_EVENTS_SKIPPED"; Tail = $tail } }
                    return @{ Result = "MISSING_VIRTIO_INPUT_EVENTS"; Tail = $tail }
                  }
                }
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
        }

        return @{ Result = "PASS"; Tail = $tail }
      }
      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|FAIL") {
        return @{ Result = "FAIL"; Tail = $tail }
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
    [Parameter(Mandatory = $true)] [bool]$ModernOnly
  )

  # Determine which QEMU virtio-snd PCI device name is available and validate it supports
  # the Aero contract v1 configuration we need.
  #
  # The strict Aero INF (`aero_virtio_snd.inf`) matches only the modern virtio-snd PCI ID
  # (`PCI\VEN_1AF4&DEV_1059`) and requires `REV_01`, so we must:
  #   - force modern-only virtio-pci enumeration (`disable-legacy=on` => `DEV_1059`)
  #   - force PCI Revision ID 0x01 (`x-pci-revision=0x01` => `REV_01`)
  $deviceName = Resolve-AeroVirtioSndPciDeviceName -QemuSystem $QemuSystem
  $helpText = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName $deviceName

  if ($helpText -notmatch "(?m)^\s*disable-legacy\b") {
    throw "QEMU device '$deviceName' does not expose 'disable-legacy'. AERO-W7-VIRTIO v1 virtio-snd requires modern-only virtio-pci enumeration (DEV_1059). Upgrade QEMU."
  }
  if ($helpText -notmatch "(?m)^\s*x-pci-revision\b") {
    throw "QEMU device '$deviceName' does not expose 'x-pci-revision'. AERO-W7-VIRTIO v1 virtio-snd requires PCI Revision ID 0x01 (REV_01). Upgrade QEMU."
  }

  return "$deviceName,disable-legacy=on,x-pci-revision=0x01,audiodev=snd0"
}

function Sanitize-AeroMarkerValue {
  param(
    [Parameter(Mandatory = $true)] [string]$Value
  )
  return $Value.Replace("|", "/").Replace("`r", " ").Replace("`n", " ").Trim()
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

function Try-AeroQmpSendInputEvents {
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

      # Keyboard: press + release 'a' (qcode).
      $writer.WriteLine('{"execute":"input-send-event","arguments":{"events":[{"type":"key","data":{"down":true,"key":{"type":"qcode","data":"a"}}},{"type":"key","data":{"down":false,"key":{"type":"qcode","data":"a"}}}]}}')
      $null = $reader.ReadLine()

      # Mouse: small relative motion + left click.
      $writer.WriteLine('{"execute":"input-send-event","arguments":{"events":[{"type":"rel","data":{"axis":"x","value":5}},{"type":"rel","data":{"axis":"y","value":-3}},{"type":"btn","data":{"down":true,"button":"left"}},{"type":"btn","data":{"down":false,"button":"left"}}]}}')
      $null = $reader.ReadLine()

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
    [Parameter(Mandatory = $true)] [int]$Port
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
      $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
        @{ type = "rel"; data = @{ axis = "x"; value = 10 } },
        @{ type = "rel"; data = @{ axis = "y"; value = 5 } }
      )

      Start-Sleep -Milliseconds 50

      $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
        @{ type = "btn"; data = @{ down = $true; button = "left" } }
      )

      Start-Sleep -Milliseconds 50

      $mouseDevice = Invoke-AeroQmpInputSendEvent -Writer $writer -Reader $reader -Device $mouseDevice -Events @(
        @{ type = "btn"; data = @{ down = $false; button = "left" } }
      )

      $kbdMode = if ([string]::IsNullOrEmpty($kbdDevice)) { "broadcast" } else { "device" }
      $mouseMode = if ([string]::IsNullOrEmpty($mouseDevice)) { "broadcast" } else { "device" }
      Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|kbd_mode=$kbdMode|mouse_mode=$mouseMode"
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
  Write-Host "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|reason=$reason"
  return $false
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
$httpListener = Start-AeroSelftestHttpServer -Port $HttpPort -Path $HttpPath

try {
  $qmpPort = $null
  $qmpArgs = @()
  $needInputEvents = [bool]$WithInputEvents
  $needQmp = ($WithVirtioSnd -and $VirtioSndAudioBackend -eq "wav") -or $needInputEvents
  if ($needQmp) {
    # QMP channel:
    # - Used for graceful shutdown when using the `wav` audiodev backend (so the RIFF header is finalized).
    # - Also used for virtio-input event injection (`input-send-event`) when -WithInputEvents is set.
    try {
      $qmpPort = Get-AeroFreeTcpPort
      $qmpArgs = @(
        "-qmp", "tcp:127.0.0.1:$qmpPort,server,nowait"
      )
    } catch {
      if ($needInputEvents) {
        throw "Failed to allocate QMP port required for -WithInputEvents: $_"
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
    $nic = "virtio-net-pci,netdev=net0"
    $drive = "file=$(Quote-AeroWin7QemuKeyvalValue $DiskImagePath),if=virtio,cache=writeback"
    if ($Snapshot) { $drive += ",snapshot=on" }

    # Transitional mode is primarily an escape hatch for older QEMU builds (or intentionally
    # testing legacy driver packages). The guest selftest still expects virtio-input devices,
    # so attach virtio-keyboard-pci + virtio-mouse-pci when the QEMU binary supports them.
    $virtioInputArgs = @()
    $haveVirtioKbd = $false
    $haveVirtioMouse = $false
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-keyboard-pci"
      $haveVirtioKbd = $true
    } catch { }
    try {
      $null = Get-AeroWin7QemuDeviceHelpText -QemuSystem $QemuSystem -DeviceName "virtio-mouse-pci"
      $haveVirtioMouse = $true
    } catch { }

    if ($haveVirtioKbd -and $haveVirtioMouse) {
      $virtioInputArgs = @(
        "-device", "virtio-keyboard-pci,id=$($script:VirtioInputKeyboardQmpId)",
        "-device", "virtio-mouse-pci,id=$($script:VirtioInputMouseQmpId)"
      )
    } else {
      if ($needInputEvents) {
        throw "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but -WithInputEvents was enabled. Upgrade QEMU or omit -WithInputEvents."
      }
      Write-Warning "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci. The guest virtio-input selftest will likely FAIL. Upgrade QEMU or adjust the guest image/selftest expectations."
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

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $false
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
    ) + $virtioInputArgs + @(
      "-drive", $drive
    ) + $virtioSndArgs + $QemuExtraArgs
  } else {
    # Ensure the QEMU binary supports the modern-only + contract revision properties we rely on.
    Assert-AeroWin7QemuSupportsAeroW7VirtioContractV1 -QemuSystem $QemuSystem -WithVirtioInput
    # Force modern-only virtio-pci IDs (DEV_1041/DEV_1042/DEV_1052) per AERO-W7-VIRTIO v1.
    # The shared QEMU arg helpers also set PCI Revision ID = 0x01 so strict contract-v1
    # drivers bind under QEMU.
    $nic = New-AeroWin7VirtioNetDeviceArg -NetdevId "net0"
    $driveId = "drive0"
    $drive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $driveId -Snapshot:$Snapshot
    $blk = New-AeroWin7VirtioBlkDeviceArg -DriveId $driveId

    $kbd = "$(New-AeroWin7VirtioKeyboardDeviceArg),id=$($script:VirtioInputKeyboardQmpId)"
    $mouse = "$(New-AeroWin7VirtioMouseDeviceArg),id=$($script:VirtioInputMouseQmpId)"

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

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem -ModernOnly $true
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
      "-device", $mouse,
      "-drive", $drive,
      "-device", $blk
    ) + $virtioSndArgs + $QemuExtraArgs
  }

  Write-Host "Launching QEMU:"
  Write-Host "  $QemuSystem $($qemuArgs -join ' ')"

  $proc = Start-Process -FilePath $QemuSystem -ArgumentList $qemuArgs -PassThru -RedirectStandardError $qemuStderrPath
  $scriptExitCode = 0

  try {
    $result = Wait-AeroSelftestResult -SerialLogPath $SerialLogPath -QemuProcess $proc -TimeoutSeconds $TimeoutSeconds -HttpListener $httpListener -HttpPath $HttpPath -FollowSerial ([bool]$FollowSerial) -RequirePerTestMarkers (-not $VirtioTransitional) -RequireVirtioSndPass ([bool]$WithVirtioSnd) -RequireVirtioInputEventsPass ([bool]$needInputEvents) -QmpHost "127.0.0.1" -QmpPort $qmpPort
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

  switch ($result.Result) {
    "PASS" {
      Write-Host "PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS"
      $scriptExitCode = 0
    }
    "FAIL" {
      Write-Host "FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QEMU_EXITED" {
      $exitCode = $null
      try { $exitCode = $proc.ExitCode } catch { }
      Write-Host "FAIL: QEMU exited before selftest result marker (exit code: $exitCode)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      Write-AeroQemuStderrTail -Path $qemuStderrPath
      $scriptExitCode = 3
    }
    "TIMEOUT" {
      Write-Host "FAIL: timed out waiting for AERO_VIRTIO_SELFTEST result marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 2
    }
    "MISSING_VIRTIO_BLK" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-blk test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-input test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_INPUT_EVENTS" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-input-events marker while -WithInputEvents was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_SKIPPED" {
      Write-Host "FAIL: virtio-input-events test was skipped (flag_not_set) but -WithInputEvents was enabled (provision the guest with --test-input-events)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "VIRTIO_INPUT_EVENTS_FAILED" {
      Write-Host "FAIL: virtio-input-events test reported FAIL while -WithInputEvents was enabled"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "QMP_INPUT_INJECT_FAILED" {
      Write-Host "FAIL: failed to inject virtio-input events via QMP (ensure QMP is reachable and QEMU supports input-send-event)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-snd test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_CAPTURE" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-snd-capture test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_SND_DUPLEX" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      $scriptExitCode = 1
    }
    "MISSING_VIRTIO_NET" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-net test marker"
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

      Write-Host "FAIL: virtio-snd test was skipped ($reason) but -WithVirtioSnd was enabled"
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

      Write-Host "FAIL: virtio-snd capture test was skipped ($reason) but -WithVirtioSnd was enabled"
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

      Write-Host "FAIL: virtio-snd duplex test was skipped ($reason) but -WithVirtioSnd was enabled"
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
}

exit $scriptExitCode
