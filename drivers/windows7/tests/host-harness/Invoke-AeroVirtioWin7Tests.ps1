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
  # Win7 drivers can bind to virtio 1.0+ IDs (DEV_1041/DEV_1042).
  [Parameter(Mandatory = $false)]
  [switch]$VirtioTransitional,

  # If set, stream newly captured COM1 serial output to stdout while waiting.
  [Parameter(Mandatory = $false)]
  [switch]$FollowSerial,

  [Parameter(Mandatory = $false)]
  [int]$TimeoutSeconds = 600,

  # HTTP server port on the host. Guest reaches it at http://10.0.2.2:<port>/aero-virtio-selftest
  [Parameter(Mandatory = $false)]
  [int]$HttpPort = 18080,

  [Parameter(Mandatory = $false)]
  [string]$HttpPath = "/aero-virtio-selftest",

  # If set, attach a virtio-snd device (virtio-sound-pci / virtio-snd-pci).
  # Note: the guest selftest only runs the virtio-snd section when enabled (via `--test-snd` / `--require-snd`).
  # If the guest is configured to test virtio-snd, you should also attach a virtio-snd device via this flag.
  [Parameter(Mandatory = $false)]
  [Alias("EnableVirtioSnd")]
  [switch]$WithVirtioSnd,

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

  # Peak absolute sample threshold for VerifyVirtioSndWav (16-bit PCM).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavPeakThreshold = 200,

  # RMS threshold for VerifyVirtioSndWav (16-bit PCM).
  [Parameter(Mandatory = $false)]
  [int]$VirtioSndWavRmsThreshold = 50,

  # Extra args passed verbatim to QEMU (advanced use).
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "AeroVirtioWin7QemuArgs.ps1")
if ($VerifyVirtioSndWav) {
  if (-not $WithVirtioSnd) {
    throw "-VerifyVirtioSndWav requires -WithVirtioSnd."
  }
  if ($VirtioSndAudioBackend -ne "wav") {
    throw "-VerifyVirtioSndWav requires -VirtioSndAudioBackend wav."
  }
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

function Try-HandleAeroHttpRequest {
  param(
    [Parameter(Mandatory = $true)] $Listener,
    [Parameter(Mandatory = $true)] [string]$Path
  )

  if (-not $Listener.Pending()) { return $false }

  $client = $Listener.AcceptTcpClient()
  try {
    $stream = $client.GetStream()
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::ASCII, $false, 4096, $true)
    $requestLine = $reader.ReadLine()
    if ($null -eq $requestLine) { return $true }

    # Drain headers.
    while ($true) {
      $line = $reader.ReadLine()
      if ($null -eq $line -or $line.Length -eq 0) { break }
    }

    $ok = $false
    if ($requestLine -match "^GET\s+(\S+)\s+HTTP/") {
      $reqPath = $Matches[1]
      if ($reqPath -eq $Path) { $ok = $true }
    }

    $body = if ($ok) { "OK`n" } else { "NOT_FOUND`n" }
    $statusLine = if ($ok) { "HTTP/1.1 200 OK" } else { "HTTP/1.1 404 Not Found" }
    $bodyBytes = [System.Text.Encoding]::ASCII.GetBytes($body)
    $hdr = @(
      $statusLine,
      "Content-Type: text/plain",
      "Content-Length: $($bodyBytes.Length)",
      "Connection: close",
      "",
      ""
    ) -join "`r`n"

    $hdrBytes = [System.Text.Encoding]::ASCII.GetBytes($hdr)
    $stream.Write($hdrBytes, 0, $hdrBytes.Length)
    $stream.Write($bodyBytes, 0, $bodyBytes.Length)
    $stream.Flush()
    return $true
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
    [Parameter(Mandatory = $true)] [bool]$RequireVirtioSndPass
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $pos = 0L
  $tail = ""
  $sawVirtioInputPass = $false
  $sawVirtioInputFail = $false
  $sawVirtioSndPass = $false
  $sawVirtioSndSkip = $false
  $sawVirtioSndFail = $false

  while ((Get-Date) -lt $deadline) {
    $null = Try-HandleAeroHttpRequest -Listener $HttpListener -Path $HttpPath

    $chunk = Read-NewText -Path $SerialLogPath -Position ([ref]$pos)
    if ($chunk.Length -gt 0) {
      if ($FollowSerial) {
        [Console]::Out.Write($chunk)
      }

      $tail += $chunk
      if ($tail.Length -gt 131072) { $tail = $tail.Substring($tail.Length - 131072) }

      if (-not $sawVirtioInputPass -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|PASS") {
        $sawVirtioInputPass = $true
      }
      if (-not $sawVirtioInputFail -and $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|FAIL") {
        $sawVirtioInputFail = $true
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

      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|PASS") {
        if ($RequirePerTestMarkers) {
          # Ensure we saw the virtio-input test marker so older selftest binaries (blk/net-only)
          # cannot accidentally pass the host harness.
          if ($sawVirtioInputFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if (-not $sawVirtioInputPass) {
            return @{ Result = "MISSING_VIRTIO_INPUT"; Tail = $tail }
          }

          # Also ensure the virtio-snd marker is present, so older selftest binaries that predate
          # virtio-snd testing cannot accidentally pass.
          if ($sawVirtioSndFail) {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          if ($sawVirtioSndPass) {
            return @{ Result = "PASS"; Tail = $tail }
          }
          if ($sawVirtioSndSkip) {
            if ($RequireVirtioSndPass) {
              return @{ Result = "VIRTIO_SND_SKIPPED"; Tail = $tail }
            }
            return @{ Result = "PASS"; Tail = $tail }
          }
          return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
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
    [Parameter(Mandatory = $true)] [string]$QemuSystem
  )

  # Determine if QEMU supports the modern virtio `disable-legacy` property for virtio-snd.
  # If QEMU doesn't support virtio-snd at all, fail early with a clear error.
  $deviceName = Resolve-AeroVirtioSndPciDeviceName -QemuSystem $QemuSystem
  $help = & $QemuSystem -device "$deviceName,help" 2>&1
  $exitCode = $LASTEXITCODE
  if ($exitCode -ne 0) {
    $helpText = ($help | Out-String).Trim()
    throw "virtio-snd device '$deviceName' is not supported by this QEMU binary ($QemuSystem). Output:`n$helpText"
  }

  $helpText = $help -join "`n"
  $device = "$deviceName,audiodev=snd0"
  if ($helpText -match "disable-legacy") {
    $device += ",disable-legacy=on"
  }
  if ($helpText -match "x-pci-revision") {
    $device += ",x-pci-revision=0x01"
  } else {
    throw "virtio-snd device '$deviceName' does not support x-pci-revision (required for Aero contract v1). Upgrade QEMU or omit -WithVirtioSnd."
  }
  return $device
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
          $skip = $chunkSize - 16
          if ($skip -gt 0) { $fs.Seek($skip, [System.IO.SeekOrigin]::Current) | Out-Null }
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

      if ($formatTag -ne 1) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_format_tag_$formatTag"
        return $false
      }
      if ($bitsPerSample -ne 16) {
        Write-Host "$markerPrefix|FAIL|reason=unsupported_bits_per_sample_$bitsPerSample"
        return $false
      }
      if ($dataSize -le 0) {
        Write-Host "$markerPrefix|FAIL|reason=missing_or_empty_data_chunk"
        return $false
      }

      $fs.Seek($dataOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
      $peak = 0
      $sumSq = 0.0
      $sampleValues = 0L
      $remainingData = $dataSize
      $buf = New-Object byte[] 65536
      $carry = $null

      while ($remainingData -gt 0) {
        $toRead = [int][Math]::Min($buf.Length, $remainingData)
        $n = $fs.Read($buf, 0, $toRead)
        if ($n -le 0) { break }
        $remainingData -= $n

        $start = 0
        if ($null -ne $carry) {
          if ($n -ge 1) {
            $val = ([int]$carry) -bor (([int]$buf[0]) -shl 8)
            if ($val -ge 0x8000) { $val -= 0x10000 }
            $absVal = if ($val -lt 0) { -$val } else { $val }
            if ($absVal -gt $peak) { $peak = $absVal }
            $sumSq += [double]($val * $val)
            $sampleValues++
            $start = 1
            $carry = $null
          } else {
            break
          }
        }

        $limit = $start + (($n - $start) / 2) * 2
        for ($i = $start; $i -lt $limit; $i += 2) {
          $val = ([int]$buf[$i]) -bor (([int]$buf[$i + 1]) -shl 8)
          if ($val -ge 0x8000) { $val -= 0x10000 }
          $absVal = if ($val -lt 0) { -$val } else { $val }
          if ($absVal -gt $peak) { $peak = $absVal }
          $sumSq += [double]($val * $val)
          $sampleValues++
        }

        if ($limit -lt $n) {
          $carry = $buf[$limit]
        }
      }

      $rms = if ($sampleValues -gt 0) { [Math]::Sqrt($sumSq / [double]$sampleValues) } else { 0.0 }
      $rmsI = [int][Math]::Round($rms)
      $frames = if ($channels -gt 0) { [long]($sampleValues / $channels) } else { [long]$sampleValues }

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
$httpListener = Start-AeroSelftestHttpServer -Port $HttpPort -Path $HttpPath

try {
  $serialChardev = "file,id=charserial0,path=$SerialLogPath"
  $netdev = "user,id=net0"
  if ($VirtioTransitional) {
    if ($WithVirtioSnd -or (-not [string]::IsNullOrEmpty($VirtioSndWavPath)) -or $VirtioSndAudioBackend -ne "none") {
      throw "-VirtioTransitional is incompatible with virtio-snd options. Remove -VirtioTransitional or pass a custom device via -QemuExtraArgs."
    }

    $nic = "virtio-net-pci,netdev=net0"
    $drive = "file=$DiskImagePath,if=virtio,cache=writeback"
    if ($Snapshot) { $drive += ",snapshot=on" }

    $qemuArgs = @(
      "-m", "$MemoryMB",
      "-smp", "$Smp",
      "-display", "none",
      "-no-reboot",
      "-chardev", $serialChardev,
      "-serial", "chardev:charserial0",
      "-netdev", $netdev,
      "-device", $nic,
      "-drive", $drive
    ) + $QemuExtraArgs
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

    $kbd = New-AeroWin7VirtioKeyboardDeviceArg
    $mouse = New-AeroWin7VirtioMouseDeviceArg

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

          # Quote the path so Start-Process (PowerShell 5.1) doesn't split it on spaces.
          $audiodev = 'wav,id=snd0,path="' + $VirtioSndWavPath + '"'
        }
        default {
          throw "Unexpected VirtioSndAudioBackend: $VirtioSndAudioBackend"
        }
      }

      $virtioSndDevice = Get-AeroVirtioSoundDeviceArg -QemuSystem $QemuSystem
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
      "-no-reboot",
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

  $proc = Start-Process -FilePath $QemuSystem -ArgumentList $qemuArgs -PassThru
  $scriptExitCode = 0

  try {
    $result = Wait-AeroSelftestResult -SerialLogPath $SerialLogPath -QemuProcess $proc -TimeoutSeconds $TimeoutSeconds -HttpListener $httpListener -HttpPath $HttpPath -FollowSerial ([bool]$FollowSerial) -RequirePerTestMarkers (-not $VirtioTransitional) -RequireVirtioSndPass ([bool]$WithVirtioSnd)
  } finally {
    if (-not $proc.HasExited) {
      Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
      try { $proc.WaitForExit(5000) } catch { }
      if (-not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        try { $proc.WaitForExit(5000) } catch { }
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
    "MISSING_VIRTIO_INPUT" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-input test marker"
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
    "VIRTIO_SND_SKIPPED" {
      $reason = "unknown"
      if ($result.Tail -match "AERO_VIRTIO_SELFTEST\\|TEST\\|virtio-snd\\|SKIP\\|flag_not_set") {
        $reason = "guest_not_configured_with_--test-snd"
      } elseif ($result.Tail -match "AERO_VIRTIO_SELFTEST\\|TEST\\|virtio-snd\\|SKIP\\|disabled") {
        $reason = "--disable-snd"
      }

      Write-Host "FAIL: virtio-snd test was skipped ($reason) but -WithVirtioSnd was enabled"
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
