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
  # Note: the guest selftest includes a virtio-snd WASAPI smoke test by default; if you do not attach a virtio-snd
  # device, either enable this flag or provision the guest to run the selftest with --disable-snd.
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

  # Extra args passed verbatim to QEMU (advanced use).
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "AeroVirtioWin7QemuArgs.ps1")

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
    [Parameter(Mandatory = $true)] [bool]$FollowSerial
  )

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  $pos = 0L
  $tail = ""

  while ((Get-Date) -lt $deadline) {
    $null = Try-HandleAeroHttpRequest -Listener $HttpListener -Path $HttpPath

    $chunk = Read-NewText -Path $SerialLogPath -Position ([ref]$pos)
    if ($chunk.Length -gt 0) {
      if ($FollowSerial) {
        [Console]::Out.Write($chunk)
      }

      $tail += $chunk
      if ($tail.Length -gt 131072) { $tail = $tail.Substring($tail.Length - 131072) }

      if ($tail -match "AERO_VIRTIO_SELFTEST\|RESULT\|PASS") {
        # Ensure we saw the virtio-input test marker so older selftest binaries (blk/net-only)
        # cannot accidentally pass the host harness.
        if ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|PASS") {
          # Also ensure the virtio-snd marker is present, so older selftest binaries that predate
          # virtio-snd testing cannot accidentally pass.
          if ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|PASS" -or $tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|SKIP") {
            return @{ Result = "PASS"; Tail = $tail }
          }
          if ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-snd\|FAIL") {
            return @{ Result = "FAIL"; Tail = $tail }
          }
          return @{ Result = "MISSING_VIRTIO_SND"; Tail = $tail }
        }
        if ($tail -match "AERO_VIRTIO_SELFTEST\|TEST\|virtio-input\|FAIL") {
          return @{ Result = "FAIL"; Tail = $tail }
        }
        return @{ Result = "MISSING_VIRTIO_INPUT"; Tail = $tail }
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
  # Force modern-only virtio-pci IDs (DEV_1041/DEV_1042) per AERO-W7-VIRTIO v1.
  # The shared QEMU arg helpers also set PCI Revision ID = 0x01 so strict contract-v1
  # drivers bind under QEMU.
  $nic = New-AeroWin7VirtioNetDeviceArg -NetdevId "net0"
  $driveId = "drive0"
  $drive = New-AeroWin7VirtioBlkDriveArg -DiskImagePath $DiskImagePath -DriveId $driveId -Snapshot:$Snapshot
  $blk = New-AeroWin7VirtioBlkDeviceArg -DriveId $driveId

  $kbd = "virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01"
  $mouse = "virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01"

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

  Write-Host "Launching QEMU:"
  Write-Host "  $QemuSystem $($qemuArgs -join ' ')"

  $proc = Start-Process -FilePath $QemuSystem -ArgumentList $qemuArgs -PassThru

  try {
    $result = Wait-AeroSelftestResult -SerialLogPath $SerialLogPath -QemuProcess $proc -TimeoutSeconds $TimeoutSeconds -HttpListener $httpListener -HttpPath $HttpPath -FollowSerial ([bool]$FollowSerial)
  } finally {
    if (-not $proc.HasExited) {
      Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
  }

  switch ($result.Result) {
    "PASS" {
      Write-Host "PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS"
      exit 0
    }
    "FAIL" {
      Write-Host "FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "QEMU_EXITED" {
      $exitCode = $null
      try { $exitCode = $proc.ExitCode } catch { }
      Write-Host "FAIL: QEMU exited before selftest result marker (exit code: $exitCode)"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 3
    }
    "TIMEOUT" {
      Write-Host "FAIL: timed out waiting for AERO_VIRTIO_SELFTEST result marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 2
    }
    "MISSING_VIRTIO_INPUT" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-input test marker"
      if ($SerialLogPath -and (Test-Path -LiteralPath $SerialLogPath)) {
        Write-Host "`n--- Serial tail ---"
        Get-Content -LiteralPath $SerialLogPath -Tail 200 -ErrorAction SilentlyContinue
      }
      exit 1
    }
    "MISSING_VIRTIO_SND" {
      Write-Host "FAIL: selftest RESULT=PASS but did not emit virtio-snd test marker"
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
      exit 4
    }
  }
} finally {
  if ($httpListener) {
    try { $httpListener.Stop() } catch { }
  }
}
