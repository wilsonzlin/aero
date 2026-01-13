<#
.SYNOPSIS
Inject (stage) the Aero virtio-input driver into a Windows 7 WIM (install.wim) or an
already-installed offline Windows directory using DISM.

.DESCRIPTION
This script automates the manual DISM steps documented in README.md:

  - WIM mode:
      dism /Mount-Wim
      dism /Image:<mount> /Add-Driver ...
      dism /Image:<mount> /Get-Drivers (+ /Get-DriverInfo verification)
      dism /Unmount-Wim /Commit (or /Discard on failure)

  - Offline directory mode:
      dism /Image:<OfflineDir> /Add-Driver ...
      dism /Image:<OfflineDir> /Get-Drivers (+ /Get-DriverInfo verification)

Hardening:
  - Input validation (paths + aero_virtio_input.inf must exist)
  - Best-effort cleanup on failures (unmount/discard + dism /Cleanup-Wim guidance)
  - Safe temp mount dir removal (only deletes temp dirs it created under %TEMP%)

.PARAMETER WimPath
Path to install.wim (or any WIM). Use with -Index for WIM mode.

.PARAMETER Index
WIM index to mount (the edition you actually install).

.PARAMETER MountDir
Directory to mount the WIM into. If omitted, a temporary directory under %TEMP% is created.

.PARAMETER Commit
When -WimPath is used, controls whether to commit the WIM on success (default: $true).
Use -Commit:$false to always discard changes after validation (dry run).

.PARAMETER OfflineDir
Root of an offline Windows installation (must contain Windows\). Example: W:\ from a mounted VHD.

.PARAMETER DriverDir
Directory containing aero_virtio_input.inf (and the matching .sys/.cat). DISM is invoked with /Recurse.

.PARAMETER ForceUnsigned
Pass /ForceUnsigned to DISM (test images only). This can stage an unsigned driver but does not
guarantee it will load on Win7 x64 without test-signing / policy changes.

.PARAMETER Help
Print usage information.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File inject-driver.ps1 -WimPath C:\win7\sources\install.wim -Index 1 -DriverDir C:\pkg\x64

.EXAMPLE
powershell -ExecutionPolicy Bypass -File inject-driver.ps1 -OfflineDir W:\ -DriverDir C:\pkg\x64

.EXAMPLE
# Dry run: mount, add, verify, but discard changes even on success.
powershell -ExecutionPolicy Bypass -File inject-driver.ps1 -WimPath C:\win7\sources\install.wim -Index 1 -DriverDir C:\pkg\x64 -Commit:$false
#>

[CmdletBinding()]
param(
  [string]$WimPath,
  [int]$Index,
  [string]$MountDir,

  [string]$OfflineDir,

  [string]$DriverDir,
  [switch]$ForceUnsigned,

  [bool]$Commit = $true,

  [switch]$Help
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$scriptPath = $MyInvocation.MyCommand.Path
$scriptName = Split-Path -Leaf $scriptPath
$tempMountPrefix = 'aero-virtio-input-wim-mount-'

function Show-Usage {
  Write-Host @"
$scriptName - inject Aero virtio-input driver using DISM

WIM mode (slipstream into install.wim):
  powershell -ExecutionPolicy Bypass -File $scriptName -WimPath C:\win7\sources\install.wim -Index 1 -DriverDir C:\path\to\pkg\x64

Offline directory mode (already-installed Windows, e.g. mounted VHD/VHDX drive letter):
  powershell -ExecutionPolicy Bypass -File $scriptName -OfflineDir W:\ -DriverDir C:\path\to\pkg\x64

Common parameters:
  -DriverDir       Directory containing aero_virtio_input.inf
  -ForceUnsigned   Pass /ForceUnsigned (test-only images; see README signing warnings)

WIM-only parameters:
  -MountDir        Mount directory (default: temp dir under %TEMP%)
  -Commit          Commit changes on success (default: True). Use -Commit:`$false to discard even on success.

Help:
  powershell -ExecutionPolicy Bypass -File $scriptName -Help
  Get-Help -Detailed "$scriptPath"
"@
}

function Test-IsAdministrator {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  $p = New-Object Security.Principal.WindowsPrincipal($id)
  return $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-FullPath([string]$Path) {
  if ([string]::IsNullOrEmpty($Path)) {
    throw "Internal error: path is empty."
  }
  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }
  return [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $Path))
}

function Test-SafeTempMountDirToRemove([string]$Path) {
  $full = Get-FullPath $Path
  $tempRoot = Get-FullPath ([System.IO.Path]::GetTempPath())

  if (-not $tempRoot.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
    $tempRoot = $tempRoot + [System.IO.Path]::DirectorySeparatorChar
  }

  if ($full.Length -lt $tempRoot.Length) {
    return $false
  }

  if ($full.Substring(0, $tempRoot.Length).ToLowerInvariant() -ne $tempRoot.ToLowerInvariant()) {
    return $false
  }

  $leaf = Split-Path -Leaf $full
  if ([string]::IsNullOrEmpty($leaf)) {
    return $false
  }

  return $leaf.ToLowerInvariant().StartsWith($tempMountPrefix.ToLowerInvariant())
}

function Invoke-Dism([string[]]$Arguments) {
  if (-not (Get-Command dism.exe -ErrorAction SilentlyContinue)) {
    throw "dism.exe was not found in PATH. Run this on a Windows host with DISM available."
  }

  $pretty = 'dism.exe ' + (($Arguments | ForEach-Object {
    if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
  }) -join ' ')

  Write-Host ''
  Write-Host ">>> $pretty"

  $output = & dism.exe @Arguments 2>&1
  $exitCode = $LASTEXITCODE

  foreach ($line in $output) {
    Write-Host $line
  }

  if ($exitCode -ne 0) {
    throw ("DISM failed with exit code {0}.{1}Command: {2}" -f $exitCode, [Environment]::NewLine, $pretty)
  }

  return $output
}

function Try-InvokeDismNoThrow([string[]]$Arguments) {
  try {
    Invoke-Dism $Arguments | Out-Null
  } catch {
    Write-Warning ("Best-effort DISM cleanup step failed: {0}" -f $_.Exception.Message)
  }
}

function Get-VirtioInputPublishedNamesFromDriversTable([string[]]$DismOutputLines) {
  foreach ($line in $DismOutputLines) {
    if ($line -match '(?i)^\s*(oem\d+\.inf)\s+.*\baero_virtio_input\.inf\b') {
      $matches[1]
    }
  }
}

function Verify-VirtioInputDriverStaged([string]$ImageRoot) {
  Write-Host ''
  Write-Host "=== Verification: DISM driver list for image '$ImageRoot' ==="

  $driversTable = Invoke-Dism @("/Image:$ImageRoot", '/Get-Drivers', '/Format:Table')
  $publishedNames = @(Get-VirtioInputPublishedNamesFromDriversTable $driversTable)

  if ($publishedNames.Count -eq 0) {
    throw @"
Verification failed: DISM did not report aero_virtio_input.inf in the offline DriverStore.

Common causes:
  - Architecture mismatch (injecting x64 driver into x86 image, or vice-versa)
  - INF hardware IDs do not match the virtio device revision (requires REV_01 for this driver)
  - Signature policy rejected the package (try signing the package; /ForceUnsigned is test-only)

See the DISM output above and README.md for troubleshooting.
"@
  }

  Write-Host ''
  Write-Host ("virtio-input appears staged as: {0}" -f ($publishedNames -join ', '))

  foreach ($pub in $publishedNames) {
    Write-Host ''
    Write-Host "=== Verification: DISM driver info for $pub ==="
    Invoke-Dism @("/Image:$ImageRoot", '/Get-DriverInfo', "/Driver:$pub") | Out-Null
  }
}

function Assert-LeafFileExists([string]$Path, [string]$What) {
  if ([string]::IsNullOrEmpty($Path)) {
    throw "Missing required parameter: $What"
  }
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw ("{0} not found: {1}" -f $What, $Path)
  }
}

function Assert-DirectoryExists([string]$Path, [string]$What) {
  if ([string]::IsNullOrEmpty($Path)) {
    throw "Missing required parameter: $What"
  }
  if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
    throw ("{0} directory not found: {1}" -f $What, $Path)
  }
}

try {
  if ($Help) {
    Show-Usage
    exit 0
  }

  if (-not (Test-IsAdministrator)) {
    throw "This script must be run elevated (Run as administrator). DISM servicing requires admin privileges."
  }

  $mode = $null
  if (-not [string]::IsNullOrEmpty($WimPath)) { $mode = 'Wim' }
  if (-not [string]::IsNullOrEmpty($OfflineDir)) {
    if ($mode) { throw "Specify exactly one of -WimPath or -OfflineDir." }
    $mode = 'OfflineDir'
  }

  if (-not $mode) {
    Show-Usage
    throw "Specify -WimPath (WIM mode) or -OfflineDir (offline directory mode)."
  }

  Assert-DirectoryExists $DriverDir 'DriverDir'
  $infPath = Join-Path $DriverDir 'aero_virtio_input.inf'
  Assert-LeafFileExists $infPath 'aero_virtio_input.inf (in DriverDir)'

  if ($mode -eq 'Wim') {
    Assert-LeafFileExists $WimPath 'WimPath'
    if ($Index -le 0) {
      throw "Index must be a positive integer in WIM mode."
    }
    if (-not [string]::IsNullOrEmpty($OfflineDir)) {
      throw "Do not use -OfflineDir with -WimPath."
    }

    $createdMountDir = $false
    $mounted = $false
    $success = $false

    if ([string]::IsNullOrEmpty($MountDir)) {
      $MountDir = Join-Path ([System.IO.Path]::GetTempPath()) ($tempMountPrefix + ([Guid]::NewGuid().ToString('N')))
      New-Item -ItemType Directory -Path $MountDir -Force | Out-Null
      $createdMountDir = $true
    } else {
      if (-not (Test-Path -LiteralPath $MountDir -PathType Container)) {
        New-Item -ItemType Directory -Path $MountDir -Force | Out-Null
      }
    }

    $addDriverArgs = @("/Image:$MountDir", '/Add-Driver', "/Driver:$DriverDir", '/Recurse')
    if ($ForceUnsigned) {
      $addDriverArgs += '/ForceUnsigned'
    }

    try {
      Invoke-Dism @('/Mount-Wim', "/WimFile:$WimPath", "/Index:$Index", "/MountDir:$MountDir") | Out-Null
      $mounted = $true

      Invoke-Dism $addDriverArgs | Out-Null
      Verify-VirtioInputDriverStaged $MountDir
      $success = $true
    } finally {
      if ($mounted) {
        $unmountArgs = @('/Unmount-Wim', "/MountDir:$MountDir")
        if ($success -and $Commit) {
          $unmountArgs += '/Commit'
        } else {
          $unmountArgs += '/Discard'
        }

        try {
          Invoke-Dism $unmountArgs | Out-Null
        } catch {
          Write-Warning ("Failed to unmount WIM at '{0}': {1}" -f $MountDir, $_.Exception.Message)
          Write-Warning "Attempting: dism.exe /Cleanup-Wim"
          Try-InvokeDismNoThrow @('/Cleanup-Wim')
          Write-Warning @"
Recovery steps (run in an elevated prompt):
  dism.exe /Get-MountedWimInfo
  dism.exe /Unmount-Wim /MountDir:"$MountDir" /Discard
  dism.exe /Cleanup-Wim
"@
          throw
        }
      } else {
        # If mount failed, a stale mount can still exist. Suggest cleanup.
        Try-InvokeDismNoThrow @('/Cleanup-Wim')
      }

      if ($createdMountDir) {
        if (Test-SafeTempMountDirToRemove $MountDir) {
          try {
            Remove-Item -LiteralPath $MountDir -Recurse -Force
          } catch {
            Write-Warning ("Failed to remove temp mount dir '{0}': {1}" -f $MountDir, $_.Exception.Message)
          }
        } else {
          Write-Warning ("Refusing to delete mount dir (did not pass safety checks): {0}" -f $MountDir)
        }
      }
    }

    Write-Host ''
    if ($success -and $Commit) {
      Write-Host "SUCCESS: Driver injected and committed to WIM."
    } elseif ($success -and (-not $Commit)) {
      Write-Host "SUCCESS: Driver injected and verified, but changes were discarded (-Commit:`$false)."
    } else {
      # If we get here, an exception would have been thrown already.
      Write-Host "FAILED."
    }
    exit 0
  }

  if ($mode -eq 'OfflineDir') {
    Assert-DirectoryExists $OfflineDir 'OfflineDir'
    $windowsDir = Join-Path $OfflineDir 'Windows'
    Assert-DirectoryExists $windowsDir 'OfflineDir\\Windows'

    if (-not [string]::IsNullOrEmpty($MountDir)) {
      throw "Do not use -MountDir in offline directory mode."
    }
    if ($Index -ne 0) {
      # Index defaults to 0 if not set; treat non-zero as suspicious.
      Write-Warning "Ignoring -Index in offline directory mode."
    }

    $addDriverArgs = @("/Image:$OfflineDir", '/Add-Driver', "/Driver:$DriverDir", '/Recurse')
    if ($ForceUnsigned) {
      $addDriverArgs += '/ForceUnsigned'
    }

    Invoke-Dism $addDriverArgs | Out-Null
    Verify-VirtioInputDriverStaged $OfflineDir

    Write-Host ''
    Write-Host "SUCCESS: Driver injected into offline directory."
    exit 0
  }

  throw "Internal error: unknown mode '$mode'"
} catch {
  Write-Host ''
  Write-Host ("ERROR: {0}" -f $_.Exception.Message)
  exit 1
}

