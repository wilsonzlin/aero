# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Verifies that the Aero Win7 virtio-input driver INF is staged in an offline Windows image.

.DESCRIPTION
  Runs:

    dism /Image:<path> /Get-Drivers /Format:Table

  And searches the output for "aero_virtio_input" (e.g. "aero_virtio_input.inf").

  Exit codes:
    0 = staged (found)
    1 = not staged (not found)
    2 = verification failed (invalid path or DISM failure)

.PARAMETER ImagePath
  Path to a mounted offline Windows directory (root), e.g. W:\ or C:\wim\mount

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\Verify-VirtioInputStaged.ps1 -ImagePath W:\
#>

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ImagePath
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

function Normalize-ImagePath {
  param([Parameter(Mandatory = $true)][string]$Path)

  $expanded = [Environment]::ExpandEnvironmentVariables($Path)

  # DISM examples use W:\ (drive root). Accept "W:" and normalize it.
  if ($expanded -match "^[A-Za-z]:$") {
    $expanded = "$expanded\"
  }

  try {
    return (Resolve-Path -LiteralPath $expanded -ErrorAction Stop).Path
  }
  catch {
    return $expanded
  }
}

try {
  $ImagePath = Normalize-ImagePath -Path $ImagePath

  if (-not (Test-Path -LiteralPath $ImagePath -PathType Container)) {
    Write-Host "ERROR: ImagePath does not exist or is not a directory: $ImagePath"
    exit 2
  }

  $windowsDir = Join-Path $ImagePath "Windows"
  if (-not (Test-Path -LiteralPath $windowsDir -PathType Container)) {
    Write-Host "ERROR: ImagePath does not look like an offline Windows root (missing 'Windows\\' directory): $ImagePath"
    Write-Host "       Expected to find: $windowsDir"
    exit 2
  }

  Write-Host "Verifying virtio-input driver is staged..."
  Write-Host "  Image: $ImagePath"
  Write-Host ""
  Write-Host "Running: dism /Image:$ImagePath /Get-Drivers /Format:Table"
  Write-Host ""

  # Avoid reporting stale exit codes from previous native commands.
  $global:LASTEXITCODE = 0
  $out = & dism "/Image:$ImagePath" "/Get-Drivers" "/Format:Table" 2>&1
  $exitCode = $LASTEXITCODE

  if ($exitCode -ne 0) {
    Write-Host "ERROR: DISM failed (exit code $exitCode)."
    Write-Host "       Ensure you are running from an elevated Administrator prompt and that ImagePath is correct."
    Write-Host ""
    Write-Host "DISM output:"
    $out | ForEach-Object { Write-Host $_ }
    exit 2
  }

  $matches = @($out | Where-Object { $_ -match "aero_virtio_input" })
  if ($matches.Count -gt 0) {
    Write-Host "OK: aero_virtio_input appears staged in the offline image."
    Write-Host ""
    Write-Host "Matching lines:"
    $matches | ForEach-Object { Write-Host ("  {0}" -f $_) }
    exit 0
  }

  Write-Host "ERROR: virtio-input driver is NOT staged in the offline image."
  Write-Host "       DISM /Get-Drivers output did not contain 'aero_virtio_input'."
  Write-Host ""
  Write-Host "Next steps:"
  Write-Host "  - Run inject-driver.ps1 against this image using the correct package directory:"
  Write-Host "      out\\packages\\windows7\\virtio-input\\x86\\   (Win7 x86 images)"
  Write-Host "      out\\packages\\windows7\\virtio-input\\x64\\   (Win7 x64 images)"
  Write-Host "  - Then re-run this verifier:"
  Write-Host ("      {0} -ImagePath {1}" -f $MyInvocation.MyCommand.Name, $ImagePath)
  exit 1
}
catch {
  Write-Host "ERROR: Verification failed: $($_.Exception.Message)"
  exit 2
}

