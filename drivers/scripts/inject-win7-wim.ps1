[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$WimFile,

  [Parameter(Mandatory = $true)]
  [int]$Index,

  [Parameter(Mandatory = $true)]
  [string]$DriverPackRoot,

  [Parameter(Mandatory = $true)]
  [string]$CertPath,

  [string[]]$CertStores = @("ROOT", "TrustedPublisher"),

  # Optional: explicit path to win-offline-cert-injector (Task 359). If omitted, the script will:
  #   1) use it from PATH if present,
  #   2) otherwise look for a built binary under tools/win-offline-cert-injector/target/{release,debug},
  #   3) otherwise fall back to direct offline SOFTWARE hive edits.
  [string]$OfflineCertInjectorPath = "",

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

function Ensure-FileWritable {
  param([Parameter(Mandatory = $true)][string]$Path)

  $item = Get-Item -LiteralPath $Path -ErrorAction Stop
  if (($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) -ne 0) {
    Write-Host "Clearing read-only attribute: $Path"
    $item.Attributes = ($item.Attributes -band (-bnot [System.IO.FileAttributes]::ReadOnly))
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if (($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) -ne 0) {
      throw "File is still read-only and cannot be modified: $Path"
    }
  }
}

function Get-CertificateInfo {
  param([Parameter(Mandatory = $true)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path)) {
    throw "Certificate file does not exist: $Path"
  }

  $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($Path)
  if (-not $cert.Thumbprint) {
    throw "Unable to read certificate thumbprint: $Path"
  }

  return @{
    Cert = $cert
    Thumbprint = $cert.Thumbprint.ToUpperInvariant()
    RawData = $cert.RawData
  }
}

function Resolve-WinOfflineCertInjectorPath {
  param([Parameter(Mandatory = $true)][string]$ExplicitPath)

  if (-not [string]::IsNullOrWhiteSpace($ExplicitPath)) {
    $resolved = (Resolve-Path -LiteralPath $ExplicitPath -ErrorAction Stop).Path
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
      throw "win-offline-cert-injector not found at: $resolved"
    }
    return $resolved
  }

  $cmd = Get-Command win-offline-cert-injector -ErrorAction SilentlyContinue
  if ($cmd) {
    if ($cmd.Source) { return $cmd.Source }
    if ($cmd.Path) { return $cmd.Path }
  }

  # Best-effort: locate a previously-built binary under the repo checkout.
  $repoRoot = $null
  try {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\\..")).Path
  } catch {
    return $null
  }

  $candidates = @(
    (Join-Path $repoRoot "tools\\win-offline-cert-injector\\target\\release\\win-offline-cert-injector.exe"),
    (Join-Path $repoRoot "tools\\win-offline-cert-injector\\target\\debug\\win-offline-cert-injector.exe")
  )
  foreach ($candidate in $candidates) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
      return $candidate
    }
  }

  return $null
}

function Get-OfflineSoftwareHivePath {
  param([Parameter(Mandatory = $true)][string]$WindowsDir)

  $hive = Join-Path $WindowsDir "Windows\System32\config\SOFTWARE"
  if (-not (Test-Path -LiteralPath $hive)) {
    throw "Offline SOFTWARE hive not found at expected path: $hive"
  }

  return $hive
}

function Get-MissingCertStoresFromOfflineSoftwareHive {
  param(
    [Parameter(Mandatory = $true)][string]$WindowsDir,
    [Parameter(Mandatory = $true)][string[]]$Stores,
    [Parameter(Mandatory = $true)][string]$Thumbprint
  )

  $hive = Get-OfflineSoftwareHivePath -WindowsDir $WindowsDir
  $tempKey = "AERO_OFFLINE_SOFTWARE_$([Guid]::NewGuid().ToString('N'))"

  & reg.exe load "HKLM\$tempKey" $hive | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "reg.exe load failed (exit $LASTEXITCODE)."
  }

  try {
    $missing = New-Object System.Collections.Generic.List[string]
    foreach ($store in $Stores) {
      $certKey = "Registry::HKEY_LOCAL_MACHINE\$tempKey\Microsoft\SystemCertificates\$store\Certificates\$Thumbprint"
      if (-not (Test-Path -LiteralPath $certKey)) {
        $missing.Add($store)
        continue
      }

      try {
        $blob = (Get-ItemProperty -LiteralPath $certKey -Name "Blob" -ErrorAction Stop).Blob
        if ($null -eq $blob -or $blob.Length -eq 0) {
          $missing.Add($store)
        }
      } catch {
        $missing.Add($store)
      }
    }

    return $missing.ToArray()
  }
  finally {
    & reg.exe unload "HKLM\$tempKey" | Out-Host
    if ($LASTEXITCODE -ne 0) {
      throw "reg.exe unload failed (exit $LASTEXITCODE)."
    }
  }
}

function Try-Invoke-WinOfflineCertInjector {
  param(
    [Parameter(Mandatory = $true)][string]$WindowsDir,
    [Parameter(Mandatory = $true)][string]$CertPath,
    [Parameter(Mandatory = $true)][string[]]$Stores,
    [Parameter(Mandatory = $true)][string]$InjectorPath
  )

  $resolvedInjector = Resolve-WinOfflineCertInjectorPath -ExplicitPath $InjectorPath
  if (-not $resolvedInjector) {
    return $false
  }

  # Our native injector uses CryptoAPI to write the registry-backed certificate store entries
  # (including the correct `Blob` REG_BINARY values), and edits the offline SOFTWARE hive in-place.
  $args = @("--windows-dir", $WindowsDir)
  foreach ($store in $Stores) {
    $args += @("--store", $store)
  }
  $args += @("--cert", $CertPath)

  & $resolvedInjector @args | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "win-offline-cert-injector failed (exit $LASTEXITCODE): $resolvedInjector"
  }

  return $true
}

function Inject-CertificateByEditingSoftwareHive {
  param(
    [Parameter(Mandatory = $true)][string]$WindowsDir,
    [Parameter(Mandatory = $true)][string[]]$Stores,
    [Parameter(Mandatory = $true)][string]$Thumbprint,
    [Parameter(Mandatory = $true)][byte[]]$RawData
  )

  $hive = Get-OfflineSoftwareHivePath -WindowsDir $WindowsDir
  $tempKey = "AERO_OFFLINE_SOFTWARE_$([Guid]::NewGuid().ToString('N'))"

  & reg.exe load "HKLM\$tempKey" $hive | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "reg.exe load failed (exit $LASTEXITCODE)."
  }

  try {
    foreach ($store in $Stores) {
      $certKey = "Registry::HKEY_LOCAL_MACHINE\$tempKey\Microsoft\SystemCertificates\$store\Certificates\$Thumbprint"
      New-Item -Path $certKey -Force | Out-Null
      New-ItemProperty -Path $certKey -Name "Blob" -PropertyType Binary -Value $RawData -Force | Out-Null
    }
  }
  finally {
    & reg.exe unload "HKLM\$tempKey" | Out-Host
    if ($LASTEXITCODE -ne 0) {
      throw "reg.exe unload failed (exit $LASTEXITCODE)."
    }
  }
}

function Ensure-OfflineCertTrust {
  param(
    [Parameter(Mandatory = $true)][string]$WindowsDir,
    [Parameter(Mandatory = $true)][string]$CertPath,
    [Parameter(Mandatory = $true)][string[]]$Stores
  )

  $certInfo = Get-CertificateInfo -Path $CertPath
  $thumb = $certInfo.Thumbprint
  $raw = $certInfo.RawData

  $missingBefore = Get-MissingCertStoresFromOfflineSoftwareHive -WindowsDir $WindowsDir -Stores $Stores -Thumbprint $thumb
  if ($missingBefore.Count -eq 0) {
    Write-Host "Certificate already present in offline image ($($Stores -join ', ')): $thumb"
    return
  }

  Write-Host "Injecting certificate into offline image ($($Stores -join ', ')): $thumb"

  $usedExternal = Try-Invoke-WinOfflineCertInjector -WindowsDir $WindowsDir -CertPath $CertPath -Stores $Stores -InjectorPath $OfflineCertInjectorPath
  if (-not $usedExternal) {
    Inject-CertificateByEditingSoftwareHive -WindowsDir $WindowsDir -Stores $Stores -Thumbprint $thumb -RawData $raw
  }

  $missingAfter = Get-MissingCertStoresFromOfflineSoftwareHive -WindowsDir $WindowsDir -Stores $Stores -Thumbprint $thumb
  if ($missingAfter.Count -ne 0) {
    throw "Certificate injection validation failed (thumbprint $thumb). Missing stores: $($missingAfter -join ', ')"
  }
}

$wim = (Resolve-Path $WimFile).Path
$pack = (Resolve-Path $DriverPackRoot).Path
$certPathResolved = (Resolve-Path $CertPath).Path

Assert-IsAdmin

Ensure-FileWritable -Path $wim

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
  Ensure-OfflineCertTrust -WindowsDir $MountDir -CertPath $certPathResolved -Stores $CertStores
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
