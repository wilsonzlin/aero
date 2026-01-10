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

  # Extra args passed verbatim to QEMU (advanced use).
  [Parameter(Mandatory = $false)]
  [string[]]$QemuExtraArgs = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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
  $nic = "virtio-net-pci,netdev=net0"
  $drive = "file=$DiskImagePath,if=virtio,cache=writeback"
  if ($Snapshot) {
    $drive += ",snapshot=on"
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
    "-drive", $drive
  ) + $QemuExtraArgs

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
