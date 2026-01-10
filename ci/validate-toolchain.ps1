$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

<#
.SYNOPSIS
  Validates that the resolved Windows driver toolchain can generate a Windows 7
  catalog using Inf2Cat.

.DESCRIPTION
  This is a *toolchain validation spike* to answer the question:
    "Which Windows Kits / WDK version installs reliably on GitHub Actions and
     supports Inf2Cat /os:7_X86,7_X64 ?"

  The script:
    - Loads out/toolchain.json (emitted by ci/install-wdk.ps1), or falls back to
      locating tools via standard Windows Kits paths.
    - Prints resolved paths + file versions for:
        Inf2Cat.exe, signtool.exe, stampinf.exe, msbuild.exe
    - Generates a minimal dummy driver package (INF + SYS referenced by INF)
    - Runs:
        Inf2Cat /driver:<dir> /os:7_X86,7_X64
    - Fails if Inf2Cat rejects the OS list or no .cat file is produced.
#>

param(
  [Parameter()]
  [string]$ToolchainJson = (Join-Path $PSScriptRoot ".." "out" "toolchain.json"),

  [Parameter()]
  [string]$LogDir = (Join-Path $PSScriptRoot ".." "out" "toolchain-validation")
)

function Get-FileVersionInfo {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  $item = Get-Item -LiteralPath $Path -ErrorAction Stop
  return [ordered]@{
    path          = $item.FullName
    fileVersion   = $item.VersionInfo.FileVersion
    productVersion = $item.VersionInfo.ProductVersion
  }
}

function Write-ToolInfo {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Name,

    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  if (-not (Test-Path -LiteralPath $Path)) {
    throw "Required tool '$Name' not found at '$Path'."
  }

  $info = Get-FileVersionInfo -Path $Path
  Write-Host "$Name:"
  Write-Host "  Path:           $($info.path)"
  Write-Host "  FileVersion:    $($info.fileVersion)"
  Write-Host "  ProductVersion: $($info.productVersion)"
}

function Get-WindowsKitsRoot10 {
  $regPaths = @(
    "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
    "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots"
  )

  foreach ($regPath in $regPaths) {
    try {
      $props = Get-ItemProperty -Path $regPath -ErrorAction Stop
      if ($null -ne $props.KitsRoot10 -and $props.KitsRoot10 -ne "") {
        return $props.KitsRoot10
      }
    } catch {
      # ignore and continue
    }
  }

  return $null
}

function Find-KitsTool {
  param(
    [Parameter(Mandatory = $true)]
    [string]$ExeName
  )

  $kitsRoot10 = Get-WindowsKitsRoot10
  if ($null -eq $kitsRoot10 -or $kitsRoot10 -eq "") {
    return $null
  }

  $binRoot = Join-Path $kitsRoot10 "bin"
  if (-not (Test-Path $binRoot)) {
    return $null
  }

  # Prefer versioned directories (...\bin\<ver>\x86\...). Fall back to legacy (...\bin\x86\...).
  $candidates = @()
  $versionDirs = Get-ChildItem -Path $binRoot -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' }
  foreach ($dir in ($versionDirs | Sort-Object -Property Name -Descending)) {
    $maybe = Join-Path $dir.FullName "x86\$ExeName"
    if (Test-Path $maybe) {
      $candidates += $maybe
    }
  }

  $legacy = Join-Path $binRoot "x86\$ExeName"
  if (Test-Path $legacy) {
    $candidates += $legacy
  }

  if ($candidates.Count -gt 0) {
    return (Resolve-Path $candidates[0]).Path
  }

  return $null
}

function Get-JsonStringValuesRecursive {
  param(
    [Parameter(Mandatory = $true)]
    $Node
  )

  $values = @()

  if ($null -eq $Node) {
    return $values
  }

  if ($Node -is [string]) {
    return @([string]$Node)
  }

  if ($Node -is [ValueType]) {
    return $values
  }

  if ($Node -is [System.Collections.IDictionary]) {
    foreach ($value in $Node.Values) {
      $values += Get-JsonStringValuesRecursive -Node $value
    }
    return $values
  }

  if ($Node -is [System.Collections.IEnumerable] -and -not ($Node -is [string])) {
    foreach ($item in $Node) {
      $values += Get-JsonStringValuesRecursive -Node $item
    }
    return $values
  }

  foreach ($property in $Node.PSObject.Properties) {
    $values += Get-JsonStringValuesRecursive -Node $property.Value
  }

  return $values
}

function Find-ExePathInToolchainJson {
  param(
    [Parameter(Mandatory = $true)]
    $Toolchain,

    [Parameter(Mandatory = $true)]
    [string]$ExeName,

    [Parameter()]
    [string]$ToolchainJsonPath
  )

  $exeRegex = [regex]::Escape($ExeName) + '$'
  $baseDir = $null
  if (-not [string]::IsNullOrWhiteSpace($ToolchainJsonPath) -and (Test-Path -LiteralPath $ToolchainJsonPath)) {
    $baseDir = Split-Path -Parent (Resolve-Path -LiteralPath $ToolchainJsonPath).Path
  }

  $strings = Get-JsonStringValuesRecursive -Node $Toolchain
  foreach ($value in $strings) {
    if ([string]::IsNullOrWhiteSpace($value)) { continue }

    if ($value -match "(?i)(^|[\\\\/])$exeRegex") {
      $candidate = [string]$value
      if ($null -ne $baseDir -and -not [System.IO.Path]::IsPathRooted($candidate)) {
        $candidate = Join-Path -Path $baseDir -ChildPath $candidate
      }

      if (Test-Path -LiteralPath $candidate) {
        return (Resolve-Path -LiteralPath $candidate).Path
      }

      return $candidate
    }
  }

  return $null
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

$transcriptPath = Join-Path $LogDir "validate-toolchain.transcript.txt"
Start-Transcript -Path $transcriptPath -Force | Out-Null

try {
  $toolchain = $null
  if (Test-Path -LiteralPath $ToolchainJson) {
    $toolchain = Get-Content -LiteralPath $ToolchainJson -Raw | ConvertFrom-Json
    Write-Host "Loaded toolchain manifest: $ToolchainJson"
  } else {
    Write-Host "Toolchain manifest not found at '$ToolchainJson' - attempting best-effort tool discovery."
  }

  $inf2cat = $null
  $signtool = $null
  $stampinf = $null
  $msbuild = $null

  if ($null -ne $toolchain) {
    # ci/install-wdk.ps1 may emit different JSON schemas depending on environment/tooling.
    # Prefer a recursive string scan for an explicit "...\\<tool>.exe" match.
    $inf2cat = Find-ExePathInToolchainJson -Toolchain $toolchain -ExeName "Inf2Cat.exe" -ToolchainJsonPath $ToolchainJson
    $signtool = Find-ExePathInToolchainJson -Toolchain $toolchain -ExeName "signtool.exe" -ToolchainJsonPath $ToolchainJson
    $stampinf = Find-ExePathInToolchainJson -Toolchain $toolchain -ExeName "stampinf.exe" -ToolchainJsonPath $ToolchainJson
    $msbuild = Find-ExePathInToolchainJson -Toolchain $toolchain -ExeName "MSBuild.exe" -ToolchainJsonPath $ToolchainJson

    # Backwards-compat (older manifests may use lowercase field names).
    if (-not $inf2cat -and $null -ne $toolchain.inf2cat) { $inf2cat = [string]$toolchain.inf2cat }
    if (-not $signtool -and $null -ne $toolchain.signtool) { $signtool = [string]$toolchain.signtool }
    if (-not $stampinf -and $null -ne $toolchain.stampinf) { $stampinf = [string]$toolchain.stampinf }
    if (-not $msbuild -and $null -ne $toolchain.msbuild) { $msbuild = [string]$toolchain.msbuild }
  }

  if (-not $inf2cat) { $inf2cat = Find-KitsTool -ExeName "Inf2Cat.exe" }
  if (-not $signtool) { $signtool = Find-KitsTool -ExeName "signtool.exe" }
  if (-not $stampinf) { $stampinf = Find-KitsTool -ExeName "stampinf.exe" }
  if (-not $msbuild) { $msbuild = (Get-Command msbuild -ErrorAction SilentlyContinue | Select-Object -First 1).Source }

  if ($null -eq $inf2cat -or $inf2cat -eq "") { throw "Inf2Cat.exe not found (toolchain.json missing or incomplete, and discovery failed)." }
  if ($null -eq $signtool -or $signtool -eq "") { throw "signtool.exe not found (toolchain.json missing or incomplete, and discovery failed)." }
  if ($null -eq $stampinf -or $stampinf -eq "") { throw "stampinf.exe not found (toolchain.json missing or incomplete, and discovery failed)." }
  if ($null -eq $msbuild -or $msbuild -eq "") { throw "msbuild.exe not found (toolchain.json missing or incomplete, and discovery failed)." }

  Write-Host ""
  Write-Host "== Resolved tool versions =="
  Write-ToolInfo -Name "Inf2Cat.exe" -Path $inf2cat
  Write-ToolInfo -Name "signtool.exe" -Path $signtool
  Write-ToolInfo -Name "stampinf.exe" -Path $stampinf
  Write-ToolInfo -Name "msbuild.exe" -Path $msbuild

  Write-Host ""
  Write-Host "== Inf2Cat Win7 catalog generation smoke test =="

  $workRoot = Join-Path $env:TEMP ("aero-toolchain-validate-" + ([Guid]::NewGuid().ToString("n")))
  $pkgDir = Join-Path $workRoot "driver"
  New-Item -ItemType Directory -Force -Path $pkgDir | Out-Null

  $infPath = Join-Path $pkgDir "aero_dummy.inf"
  $sysPath = Join-Path $pkgDir "aero_dummy.sys"

  # A real driver binary isn't required for catalog generation; Inf2Cat hashes the files
  # referenced by the INF. Keep it deterministic and tiny.
  [byte[]]$sysBytes = 0..255
  [System.IO.File]::WriteAllBytes($sysPath, $sysBytes)

$infContents = @'
[Version]
Signature="$WINDOWS NT$"
Class=System
ClassGuid={4d36e97d-e325-11ce-bfc1-08002be10318}
Provider=%ManufacturerName%
CatalogFile=aero_dummy.cat
DriverVer=01/01/2026,1.0.0.0

[DestinationDirs]
DefaultDestDir = 12

[SourceDisksNames]
1 = %DiskName%,,,

[SourceDisksFiles]
aero_dummy.sys = 1

[Manufacturer]
%ManufacturerName%=Standard,NTx86,NTamd64

[Standard.NTx86]
%DeviceDesc%=Aero_Install, Root\AERODUMMY

[Standard.NTamd64]
%DeviceDesc%=Aero_Install, Root\AERODUMMY

[Aero_Install.NT]
CopyFiles=Aero_CopyFiles

[Aero_CopyFiles]
aero_dummy.sys

[Aero_Install.NT.Services]
AddService = AeroDummy, 0x00000002, Aero_Service_Install

[Aero_Service_Install]
DisplayName    = %ServiceName%
ServiceType    = 1
StartType      = 3
ErrorControl   = 1
ServiceBinary  = %12%\aero_dummy.sys

[Strings]
ManufacturerName="Aero"
DeviceDesc="Aero Dummy Driver (Inf2Cat validation)"
ServiceName="AeroDummy"
DiskName="Aero Dummy Install Disk"
'@

  Set-Content -LiteralPath $infPath -Value $infContents -Encoding ASCII

  Write-Host ""
  Write-Host "== INF stamping smoke test (stampinf.exe) =="

  # Catalog generation hashes INF contents; ensure our stamping workflow (DriverVer update)
  # runs cleanly before Inf2Cat.
  $stampScript = Join-Path $repoRoot "ci" "stamp-infs.ps1"
  if (Test-Path -LiteralPath $stampScript) {
    $toolchainJsonForStamp = $null
    if (Test-Path -LiteralPath $ToolchainJson) {
      $toolchainJsonForStamp = $ToolchainJson
    }

    $stampArgs = @{
      StagingDir = $pkgDir
      InfPaths = @($infPath)
      RepoRoot = $repoRoot
      # Avoid requiring git history/tags for this toolchain smoke test.
      DriverVerVersion = "1.0.0.0"
      DriverVerDate = (Get-Date)
      PackageVersion = "toolchain-smoke"
    }
    if ($toolchainJsonForStamp) {
      $stampArgs.ToolchainJson = $toolchainJsonForStamp
    } else {
      # Fall back to the resolved stampinf.exe path for local runs that don't have a manifest.
      $stampArgs.StampInfPath = $stampinf
    }

    & $stampScript @stampArgs | Out-Null
  } else {
    Write-Warning "stamp-infs.ps1 not found at '$stampScript'; stamping INF via stampinf.exe directly."
    & $stampinf -f $infPath -d (Get-Date -Format "MM/dd/yyyy") -v "1.0.0.0" | Out-Null
    if ($LASTEXITCODE -ne 0) {
      throw "stampinf.exe failed (exit $LASTEXITCODE)."
    }
  }

  $inf2catLogPath = Join-Path $LogDir "inf2cat.stdout-stderr.txt"

  $inf2catBinDir = Split-Path -Parent $inf2cat
  $env:PATH = "$inf2catBinDir;$env:PATH"

  $inf2catArgs = @(
    "/driver:$pkgDir",
    "/os:7_X86,7_X64"
  )

  Write-Host "Running: `"$inf2cat`" $($inf2catArgs -join ' ')"
  & $inf2cat @inf2catArgs 2>&1 | Tee-Object -FilePath $inf2catLogPath
  $exitCode = $LASTEXITCODE

  if ($exitCode -ne 0) {
    throw "Inf2Cat failed with exit code $exitCode. See: $inf2catLogPath"
  }

  $catFiles = Get-ChildItem -Path $pkgDir -Filter "*.cat" -File -ErrorAction SilentlyContinue
  if ($null -eq $catFiles -or $catFiles.Count -eq 0) {
    throw "Inf2Cat exited successfully but no .cat files were produced under '$pkgDir'."
  }

  foreach ($cat in $catFiles) {
    if ($cat.Length -le 0) {
      throw "Inf2Cat produced an empty catalog file: $($cat.FullName)"
    }
    Write-Host "Generated catalog: $($cat.FullName) ($($cat.Length) bytes)"
  }

  $artifactPkgDir = Join-Path $LogDir "dummy-driver-package"
  Copy-Item -Path $pkgDir -Destination $artifactPkgDir -Recurse -Force
  Write-Host "Copied dummy driver package to: $artifactPkgDir"

  Write-Host ""
  Write-Host "Toolchain validation PASSED."
} finally {
  Stop-Transcript | Out-Null
}
