[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$WimFile,

  [Parameter(Mandatory = $true)]
  [int]$Index,

  [Parameter(Mandatory = $true)]
  [string]$DriverPackRoot,

  [string]$MountDir = (Join-Path $env:TEMP "aero-wim-mount")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-IsAdmin {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  $p = New-Object Security.Principal.WindowsPrincipal($id)
  if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated Administrator PowerShell prompt."
  }
}

function Get-WimArchitecture {
  param(
    [Parameter(Mandatory = $true)]
    [string]$WimFile,
    [Parameter(Mandatory = $true)]
    [int]$Index
  )

  $out = & dism /English /Get-WimInfo /WimFile:$WimFile /Index:$Index
  foreach ($line in $out) {
    if ($line -match "^\s*Architecture\s*:\s*(.+?)\s*$") {
      return $Matches[1].Trim().ToLowerInvariant()
    }
  }

  throw "Unable to determine WIM architecture for $WimFile (index $Index)."
}

function Map-ArchToPack {
  param([Parameter(Mandatory = $true)][string]$DismArch)
  switch ($DismArch) {
    "x86" { return "x86" }
    "x64" { return "amd64" }
    "amd64" { return "amd64" }
    default { throw "Unsupported/unknown WIM architecture '$DismArch'." }
  }
}

$wim = (Resolve-Path $WimFile).Path
$pack = (Resolve-Path $DriverPackRoot).Path

Assert-IsAdmin

$arch = Map-ArchToPack (Get-WimArchitecture -WimFile $wim -Index $Index)
$drivers = Join-Path $pack "win7\$arch"

if (-not (Test-Path $drivers)) {
  throw "Driver directory does not exist: $drivers"
}

if (-not (Test-Path $MountDir)) {
  New-Item -ItemType Directory -Path $MountDir | Out-Null
}

Write-Host "Injecting drivers into WIM..."
Write-Host "  WIM:     $wim"
Write-Host "  Index:   $Index"
Write-Host "  Arch:    $arch"
Write-Host "  Drivers: $drivers"
Write-Host "  Mount:   $MountDir"

$commit = $false

try {
  & dism /English /Mount-Wim /WimFile:$wim /Index:$Index /MountDir:$MountDir | Out-Host
  & dism /English /Image:$MountDir /Add-Driver /Driver:$drivers /Recurse | Out-Host
  $commit = $true
}
finally {
  if (Test-Path (Join-Path $MountDir "Windows")) {
    if ($commit) {
      & dism /English /Unmount-Wim /MountDir:$MountDir /Commit | Out-Host
    } else {
      & dism /English /Unmount-Wim /MountDir:$MountDir /Discard | Out-Host
    }
  }
}
