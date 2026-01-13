#Requires -Version 5.1

[CmdletBinding()]
param(
  # Path to install.wim or boot.wim.
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$WimPath,

  # Image index inside the WIM (e.g. install.wim has one per edition; boot.wim index 2 is Setup/WinPE).
  [Parameter(Mandatory)]
  [ValidateRange(1, [int]::MaxValue)]
  [int]$Index,

  # Directory containing aero_virtio_input.inf/.sys (and optionally .cat).
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$DriverPackageDir,

  # Optional mount directory. If omitted, a unique temp directory is used.
  [string]$MountDir = "",

  # Pass /ForceUnsigned to DISM /Add-Driver.
  [switch]$ForceUnsigned,

  # Whether to commit the WIM modifications on unmount.
  # Default is commit; to discard changes (for debugging), run with: -Commit:$false
  [switch]$Commit = $true
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-IsAdministrator {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated PowerShell prompt (Run as Administrator)."
  }
}

function Assert-CommandAvailable {
  param([Parameter(Mandatory)][string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command '$Name' was not found in PATH."
  }
}

function Ensure-WritableFile {
  param(
    [Parameter(Mandatory)][string]$Path,
    [Parameter(Mandatory)][string]$Label
  )

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "$Label not found: $Path"
  }

  # ISO extractors commonly mark WIMs read-only; clear it so DISM can commit modifications.
  & attrib.exe -r $Path 2>$null | Out-Null

  $item = Get-Item -LiteralPath $Path -ErrorAction Stop
  if ($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) {
    throw ("$Label is read-only and cannot be serviced in-place. Copy it to a writable NTFS directory and retry.`n" +
      "Path: $Path")
  }
}

function Ensure-EmptyDirectory {
  param([Parameter(Mandatory)][string]$Path)

  if (Test-Path -LiteralPath $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
      throw "MountDir path exists but is not a directory: $Path"
    }
    $items = @(Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue)
    if ($items.Count -ne 0) {
      throw "MountDir must be empty. Directory is not empty: $Path"
    }
    return
  }

  New-Item -ItemType Directory -Path $Path | Out-Null
}

function Format-Arg {
  param([Parameter(Mandatory)][string]$Arg)
  if ($Arg -match '[\s"`]') {
    return '"' + ($Arg -replace '"', '\"') + '"'
  }
  return $Arg
}

function Invoke-NativeCommandResult {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $cmdLine = ("{0} {1}" -f $FilePath, (($ArgumentList | ForEach-Object { Format-Arg $_ }) -join " ")).Trim()
  Write-Host "`n> $cmdLine"

  $output = & $FilePath @ArgumentList 2>&1
  if (-not $SuppressOutput) {
    foreach ($line in $output) {
      Write-Host $line
    }
  }

  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ,$output
    CommandLine = $cmdLine
  }
}

function Invoke-NativeCommand {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $result = Invoke-NativeCommandResult -FilePath $FilePath -ArgumentList $ArgumentList -SuppressOutput:$SuppressOutput
  if ($result.ExitCode -ne 0) {
    $outputText = ($result.Output | Out-String).Trim()
    if ($outputText) {
      throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)`n`n$outputText"
    }
    throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)"
  }
}

function Invoke-NativeCommandWithOutput {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList
  )

  $result = Invoke-NativeCommandResult -FilePath $FilePath -ArgumentList $ArgumentList -SuppressOutput
  if ($result.ExitCode -ne 0) {
    $outputText = ($result.Output | Out-String).Trim()
    if ($outputText) {
      throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)`n`n$outputText"
    }
    throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)"
  }

  return ,$result.Output
}

function Show-InjectedDriverEntry {
  param(
    [Parameter(Mandatory)][string]$MountedImageRoot
  )

  $out = Invoke-NativeCommandWithOutput -FilePath "dism.exe" -ArgumentList @(
    "/English",
    "/Image:$MountedImageRoot",
    "/Get-Drivers",
    "/Format:Table"
  )

  $matches = @($out | Where-Object { $_ -match '(?i)aero_virtio_input\.inf' })
  if ($matches.Count -eq 0) {
    Write-Warning "Could not find 'aero_virtio_input.inf' in DISM /Get-Drivers output. If you suspect it was injected, run manually:"
    Write-Warning ("  dism /English /Image:{0} /Get-Drivers /Format:Table" -f (Format-Arg $MountedImageRoot))
    return
  }

  Write-Host "`nFound virtio-input in offline DriverStore (matching rows):"
  foreach ($line in $matches) {
    Write-Host ("  {0}" -f $line)
  }

  $published = New-Object System.Collections.Generic.List[string]
  foreach ($line in $matches) {
    if ($line -match '^\s*(oem\d+\.inf)\s+') {
      if (-not ($published -contains $Matches[1])) {
        $published.Add($Matches[1]) | Out-Null
      }
    }
  }
  if ($published.Count -gt 0) {
    Write-Host ("Published name(s): {0}" -f ($published -join ", "))
  }
}

Assert-IsAdministrator
Assert-CommandAvailable -Name "dism.exe"
Assert-CommandAvailable -Name "attrib.exe"

if (-not (Test-Path -LiteralPath $WimPath -PathType Leaf)) {
  throw "-WimPath must be an existing .wim file. Got: $WimPath"
}
if (-not (Test-Path -LiteralPath $DriverPackageDir -PathType Container)) {
  throw "-DriverPackageDir must be an existing directory. Got: $DriverPackageDir"
}

$resolvedWimPath = (Resolve-Path -LiteralPath $WimPath).Path
$resolvedDriverPackageDir = (Resolve-Path -LiteralPath $DriverPackageDir).Path

Ensure-WritableFile -Path $resolvedWimPath -Label "WIM file"

$infPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.inf"
$sysPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.sys"
$catPath = Join-Path -Path $resolvedDriverPackageDir -ChildPath "aero_virtio_input.cat"

if (-not (Test-Path -LiteralPath $infPath -PathType Leaf)) {
  throw ("-DriverPackageDir must contain 'aero_virtio_input.inf'.`n" +
    "Expected: $infPath")
}
if (-not (Test-Path -LiteralPath $sysPath -PathType Leaf)) {
  throw ("-DriverPackageDir must contain 'aero_virtio_input.sys'.`n" +
    "Expected: $sysPath")
}
if (-not (Test-Path -LiteralPath $catPath -PathType Leaf)) {
  if (-not $ForceUnsigned) {
    Write-Warning "Driver package is missing aero_virtio_input.cat. DISM may reject the package unless you use -ForceUnsigned (test-only)."
  }
}

$mountDirProvided = -not [string]::IsNullOrWhiteSpace($MountDir)
if (-not $mountDirProvided) {
  $MountDir = Join-Path -Path $env:TEMP -ChildPath ("aero-virtio-input-mount-" + [Guid]::NewGuid().ToString("N"))
}

$resolvedMountDir = [System.IO.Path]::GetFullPath($MountDir)

Write-Host "========================================"
Write-Host "virtio-input DISM injection plan"
Write-Host "========================================"
Write-Host "WIM                 : $resolvedWimPath"
Write-Host "Index               : $Index"
Write-Host "DriverPackageDir    : $resolvedDriverPackageDir"
Write-Host "ForceUnsigned       : $(if ($ForceUnsigned) { "ON" } else { "OFF" })"
Write-Host "Commit on unmount    : $(if ($Commit) { "ON" } else { "OFF (discard changes)" })"
Write-Host "MountDir            : $resolvedMountDir"
Write-Host "========================================"

$mounted = $false
$unmounted = $false
$hadError = $false

try {
  Ensure-EmptyDirectory -Path $resolvedMountDir

  Write-Host "`nMounting WIM..."
  Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList @(
    "/English",
    "/Mount-Wim",
    ("/WimFile:$resolvedWimPath"),
    ("/Index:$Index"),
    ("/MountDir:$resolvedMountDir")
  )
  $mounted = $true

  Write-Host "`nAdding driver..."
  $addArgs = @(
    "/English",
    ("/Image:$resolvedMountDir"),
    "/Add-Driver",
    ("/Driver:$resolvedDriverPackageDir"),
    "/Recurse"
  )
  if ($ForceUnsigned) {
    $addArgs += "/ForceUnsigned"
  }
  Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList $addArgs

  # Optional verification step: print the matching /Get-Drivers row(s) for aero_virtio_input.inf.
  Show-InjectedDriverEntry -MountedImageRoot $resolvedMountDir
}
catch {
  $hadError = $true
  throw
}
finally {
  if ($mounted) {
    $unmountArgs = @(
      "/English",
      "/Unmount-Wim",
      ("/MountDir:$resolvedMountDir")
    )

    if (-not $hadError -and $Commit) {
      Write-Host "`nUnmounting (commit)..."
      $unmountArgs += "/Commit"
    }
    else {
      if ($hadError) {
        Write-Warning "`nUnmounting (discard) due to earlier failure..."
      }
      else {
        Write-Warning "`nUnmounting (discard) because -Commit is OFF..."
      }
      $unmountArgs += "/Discard"
    }

    try {
      Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList $unmountArgs
      $unmounted = $true
    }
    catch {
      if ($hadError) {
        Write-Warning "DISM failed to unmount the image. The WIM may still be mounted at: $resolvedMountDir"
        Write-Warning "You may need to run: dism /Cleanup-Wim"
      }
      else {
        throw
      }
    }
  }

  # Always try to clean up the mount directory when we know the image is not mounted anymore.
  if (-not $mounted -or $unmounted) {
    try {
      if (Test-Path -LiteralPath $resolvedMountDir) {
        Remove-Item -LiteralPath $resolvedMountDir -Recurse -Force -ErrorAction Stop
      }
    }
    catch {
      Write-Warning "Failed to delete mount directory '$resolvedMountDir'. You can remove it manually after ensuring the image is unmounted."
      if (-not $hadError) {
        throw
      }
    }
  }
  elseif (Test-Path -LiteralPath $resolvedMountDir) {
    Write-Warning "Keeping mount directory because the image may still be mounted: $resolvedMountDir"
  }
}

Write-Host "`nDone."
