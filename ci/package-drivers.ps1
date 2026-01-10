[CmdletBinding()]
param(
    [string] $InputRoot = "out/packages",
    [string] $CertPath = "out/certs/aero-test.cer",
    [string] $OutDir = "out/artifacts",
    [string] $Version,
    [switch] $NoIso,
    [switch] $MakeFatImage,
    [switch] $FatImageStrict,
    [int] $FatImageSizeMB = 64
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoPath {
    param([Parameter(Mandatory = $true)][string] $Path)

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }

    $repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
    return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Get-VersionString {
    function Parse-SemverTag {
        param([Parameter(Mandatory = $true)][string] $Tag)

        $clean = $Tag.Trim()
        if ($clean.StartsWith("v")) {
            $clean = $clean.Substring(1)
        }

        if ($clean -match "^([0-9]+)\\.([0-9]+)\\.([0-9]+)$") {
            return [pscustomobject]@{
                Major = [int] $Matches[1]
                Minor = [int] $Matches[2]
                Patch = [int] $Matches[3]
                Text = $clean
            }
        }

        return $null
    }

    $repoRoot = $null
    $sha = $null
    $commitDate = $null
    $tag = $null

    try {
        $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
        $sha = (& git -C $repoRoot rev-parse --short=12 HEAD 2>$null).Trim()
        $commitDateIso = (& git -C $repoRoot show -s --format=%cI HEAD 2>$null).Trim()
        if (-not [string]::IsNullOrWhiteSpace($commitDateIso)) {
            $commitDate = [DateTimeOffset]::Parse($commitDateIso, [System.Globalization.CultureInfo]::InvariantCulture)
        }
        $tag = (& git -C $repoRoot describe --tags --abbrev=0 --match "v[0-9]*" 2>$null).Trim()
        if ([string]::IsNullOrWhiteSpace($tag)) {
            $tag = $null
        }
    } catch {
        $sha = $null
        $commitDate = $null
        $tag = $null
    }

    $date = $null
    if ($null -ne $commitDate) {
        $date = $commitDate.ToString("yyyyMMdd", [System.Globalization.CultureInfo]::InvariantCulture)
    } else {
        $date = Get-Date -Format "yyyyMMdd"
    }

    if ([string]::IsNullOrWhiteSpace($sha) -or [string]::IsNullOrWhiteSpace($repoRoot)) {
        return $date
    }

    $base = $null
    if ($tag) {
        $base = Parse-SemverTag $tag
    }
    if (-not $base) {
        $base = [pscustomobject]@{ Major = 0; Minor = 0; Patch = 0; Text = "0.0.0" }
    }

    $distance = 0
    if ($tag) {
        try {
            $distance = [int] ((& git -C $repoRoot rev-list --count "$tag..HEAD" 2>$null) | Out-String).Trim()
        } catch {
            $distance = [int] ((& git -C $repoRoot rev-list --count HEAD 2>$null) | Out-String).Trim()
        }
    } else {
        $distance = [int] ((& git -C $repoRoot rev-list --count HEAD 2>$null) | Out-String).Trim()
    }

    $semver = $null
    if ($distance -eq 0) {
        $semver = "{0}+g{1}" -f $base.Text, $sha
    } else {
        $semver = "{0}+{1}.g{2}" -f $base.Text, $distance, $sha
    }

    return "$date-$semver"
}

function Get-ArchFromPath {
    param([Parameter(Mandatory = $true)][string] $Path)

    $p = $Path.ToLowerInvariant()

    if ($p -match '(^|[\\/])(amd64|x64|x86_64)([\\/]|$)') {
        return "x64"
    }

    if ($p -match '(^|[\\/])(x86|i386|win32)([\\/]|$)') {
        return "x86"
    }

    return $null
}

function Get-DriverNameFromRelativeSegments {
    param(
        [Parameter(Mandatory = $true)][string[]] $Segments,
        [Parameter(Mandatory = $true)][string] $Fallback
    )

    $skip = @(
        "drivers",
        "driver",
        "packages",
        "package",
        "signed",
        "release",
        "debug",
        "build",
        "bin",
        "dist",
        "windows",
        "wdk",
        "win7",
        "w7",
        "windows7",
        "win7sp1",
        "sp1"
    )

    # Prefer the *deepest* meaningful segment so that nested layouts like:
    #   out/packages/<group>/<driver>/<arch>/...
    # map to the expected driver name (<driver>), not the top-level group.
    for ($i = $Segments.Count - 1; $i -ge 0; $i--) {
        $segment = $Segments[$i]
        $s = $segment.ToLowerInvariant()
        if ($s -in @("x86", "i386", "win32", "x64", "amd64", "x86_64")) {
            continue
        }
        if ($s -in $skip) {
            continue
        }

        return $segment
    }

    return $Fallback
}

function Normalize-PathComponent {
    param([Parameter(Mandatory = $true)][string] $Value)

    $invalid = [System.IO.Path]::GetInvalidFileNameChars()
    foreach ($c in $invalid) {
        $Value = $Value.Replace($c, "_")
    }
    return $Value
}

function Assert-ContainsFileExtension {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Extension
    )

    $pattern = "*.$Extension"
    $found = Get-ChildItem -Path $Root -Recurse -File -Filter $pattern -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $found) {
        throw "Expected at least one '$pattern' file under '$Root'."
    }
}

function Write-InstallTxt {
    param([Parameter(Mandatory = $true)][string] $DestPath)

    $lines = @(
        "AeroVirtIO Windows 7 Driver Package",
        "",
        "This folder contains signed driver packages for Windows 7 (x86 + x64).",
        "These drivers are signed with a TEST certificate and are not suitable for production use.",
        "",
        "1) Enable test signing (Windows 7 x64 only)",
        "-------------------------------------------",
        "Open an elevated Command Prompt and run:",
        "  bcdedit /set testsigning on",
        "Reboot the machine.",
        "",
        "To disable later:",
        "  bcdedit /set testsigning off",
        "",
        "2) Import the signing certificate",
        "---------------------------------",
        "The certificate is included as: aero-test.cer",
        "",
        "Option A (GUI):",
        "  - Run: certmgr.msc",
        "  - Import aero-test.cer into BOTH:",
        "      * Trusted Root Certification Authorities",
        "      * Trusted Publishers",
        "",
        "Option B (Command line, elevated):",
        "  certutil -addstore -f Root aero-test.cer",
        "  certutil -addstore -f TrustedPublisher aero-test.cer",
        "",
        "3) Install drivers (PnPUtil)",
        "----------------------------",
        "Open an elevated Command Prompt in the folder containing the driver files and run:",
        "  pnputil -i -a <path-to-driver.inf>",
        "",
        "Example (bundle ZIP/ISO layout):",
        "  pnputil -i -a drivers\\<driver>\\x64\\<something>.inf",
        "",
        "Example (FAT disk image layout):",
        "  pnputil -i -a x64\\<driver>\\<something>.inf",
        "",
        "4) Windows Setup: \"Load driver\"",
        "--------------------------------",
        "Mount the ISO (if produced) or attach the FAT driver disk image (if produced).",
        "Then choose:",
        "  Install Windows -> Load driver -> Browse",
        "",
        "Driver locations:",
        "  - Bundle ZIP/ISO: drivers\\<driver>\\x86\\*.inf or drivers\\<driver>\\x64\\*.inf",
        "  - FAT disk image: x86\\<driver>\\*.inf or x64\\<driver>\\*.inf",
        ""
    )

    $dir = Split-Path -Parent $DestPath
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $lines | Set-Content -Path $DestPath -Encoding UTF8
}

function New-DriverArtifactRoot {
    param(
        [Parameter(Mandatory = $true)][string] $DestRoot,
        [Parameter(Mandatory = $true)][string] $CertSourcePath
    )

    New-Item -ItemType Directory -Force -Path $DestRoot | Out-Null
    Copy-Item -Path $CertSourcePath -Destination (Join-Path $DestRoot "aero-test.cer") -Force
    Write-InstallTxt -DestPath (Join-Path $DestRoot "INSTALL.txt")
}

function Copy-DriversForArch {
    param(
        [Parameter(Mandatory = $true)][string] $InputRoot,
        [Parameter(Mandatory = $true)][string[]] $Arches,
        [Parameter(Mandatory = $true)][string] $DestRoot
    )

    $inputRootTrimmed = $InputRoot.TrimEnd("\", "/")

    $infFiles = Get-ChildItem -Path $InputRoot -Recurse -File -Filter "*.inf" -ErrorAction SilentlyContinue
    if (-not $infFiles) {
        throw "No '.inf' files found under '$InputRoot'."
    }

    $seen = New-Object "System.Collections.Generic.HashSet[string]"
    $copied = 0

    foreach ($inf in $infFiles) {
        $arch = Get-ArchFromPath -Path $inf.FullName
        if (-not $arch) {
            continue
        }
        if ($Arches -notcontains $arch) {
            continue
        }

        $srcDir = Split-Path -Parent $inf.FullName
        $relative = ""
        if ($srcDir.Length -ge $inputRootTrimmed.Length -and $srcDir.StartsWith($inputRootTrimmed, [System.StringComparison]::OrdinalIgnoreCase)) {
            $relative = $srcDir.Substring($inputRootTrimmed.Length).TrimStart("\", "/")
        }
        $segments = @()
        if (-not [string]::IsNullOrWhiteSpace($relative)) {
            $segments = $relative -split "[\\/]+"
        }

        $driverName = Get-DriverNameFromRelativeSegments -Segments $segments -Fallback $inf.BaseName
        $driverName = $driverName.Trim()
        if ([string]::IsNullOrWhiteSpace($driverName)) {
            $driverName = $inf.BaseName
        }
        $driverName = Normalize-PathComponent -Value $driverName

        $destDir = Join-Path $DestRoot (Join-Path "drivers" (Join-Path $driverName $arch))
        $key = "$arch|$driverName|$srcDir"
        if ($seen.Contains($key)) {
            continue
        }
        $null = $seen.Add($key)

        New-Item -ItemType Directory -Force -Path $destDir | Out-Null
        Copy-Item -Path (Join-Path $srcDir "*") -Destination $destDir -Recurse -Force
        $copied++
    }

    if ($copied -eq 0) {
        throw "No driver packages found for architectures: $($Arches -join ', ')."
    }

    Assert-ContainsFileExtension -Root $DestRoot -Extension "inf"
    Assert-ContainsFileExtension -Root $DestRoot -Extension "cat"
    Assert-ContainsFileExtension -Root $DestRoot -Extension "sys"
}

function New-FatImageInputFromBundle {
    param(
        [Parameter(Mandatory = $true)][string] $BundleRoot,
        [Parameter(Mandatory = $true)][string] $CertSourcePath,
        [Parameter(Mandatory = $true)][string] $DestRoot
    )

    New-DriverArtifactRoot -DestRoot $DestRoot -CertSourcePath $CertSourcePath

    $x86Root = Join-Path $DestRoot "x86"
    $x64Root = Join-Path $DestRoot "x64"
    New-Item -ItemType Directory -Force -Path $x86Root | Out-Null
    New-Item -ItemType Directory -Force -Path $x64Root | Out-Null

    $bundleDrivers = Join-Path $BundleRoot "drivers"
    if (-not (Test-Path $bundleDrivers)) {
        throw "Expected a 'drivers' folder in the bundle root: '$bundleDrivers'."
    }

    $driverDirs = Get-ChildItem -Path $bundleDrivers -Directory -ErrorAction SilentlyContinue
    if (-not $driverDirs) {
        throw "No driver directories found under '$bundleDrivers'."
    }

    foreach ($driverDir in $driverDirs) {
        foreach ($arch in @("x86", "x64")) {
            $src = Join-Path $driverDir.FullName $arch
            if (-not (Test-Path $src)) {
                continue
            }

            $dest = Join-Path (Join-Path $DestRoot $arch) $driverDir.Name
            New-Item -ItemType Directory -Force -Path $dest | Out-Null
            Copy-Item -Path (Join-Path $src "*") -Destination $dest -Recurse -Force
        }
    }

    Assert-ContainsFileExtension -Root $DestRoot -Extension "inf"
    Assert-ContainsFileExtension -Root $DestRoot -Extension "cat"
    Assert-ContainsFileExtension -Root $DestRoot -Extension "sys"
}

function New-ZipFromFolder {
    param(
        [Parameter(Mandatory = $true)][string] $Folder,
        [Parameter(Mandatory = $true)][string] $ZipPath
    )

    if (Test-Path $ZipPath) {
        Remove-Item -Force $ZipPath
    }

    Compress-Archive -Path (Join-Path $Folder "*") -DestinationPath $ZipPath -Force
    $zipFile = Get-Item -Path $ZipPath -ErrorAction SilentlyContinue
    if (-not $zipFile -or $zipFile.Length -le 0) {
        throw "Failed to create ZIP, or ZIP is empty: '$ZipPath'."
    }
}

function New-IsoFromFolder {
    param(
        [Parameter(Mandatory = $true)][string] $Folder,
        [Parameter(Mandatory = $true)][string] $IsoPath,
        [Parameter(Mandatory = $true)][string] $VolumeLabel
    )

    $isWindows = $false
    try {
        $isWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
    } catch {
        $isWindows = $false
    }

    if (-not $isWindows) {
        throw "ISO creation requires Windows (IMAPI2). Re-run with -NoIso, or run this script on Windows."
    }

    if (Test-Path $IsoPath) {
        Remove-Item -Force $IsoPath
    }

    $helper = Join-Path $PSScriptRoot "lib/New-IsoFile.ps1"
    if (-not (Test-Path $helper)) {
        throw "Missing helper script: '$helper'."
    }

    $powershellExe = (Get-Command powershell.exe -ErrorAction SilentlyContinue).Source
    if ($powershellExe) {
        & $powershellExe -NoProfile -ExecutionPolicy Bypass -STA -File $helper -SourcePath $Folder -IsoPath $IsoPath -VolumeLabel $VolumeLabel
        if ($LASTEXITCODE -ne 0) {
            throw "ISO creation failed (exit code $LASTEXITCODE)."
        }
    } else {
        . $helper
        New-IsoFile -SourcePath $Folder -IsoPath $IsoPath -VolumeLabel $VolumeLabel
    }

    $isoFile = Get-Item -Path $IsoPath -ErrorAction SilentlyContinue
    if (-not $isoFile -or $isoFile.Length -le 0) {
        throw "ISO file was not created or is empty: '$IsoPath'."
    }
}

$inputRootResolved = Resolve-RepoPath -Path $InputRoot
$certPathResolved = Resolve-RepoPath -Path $CertPath
$outDirResolved = Resolve-RepoPath -Path $OutDir

if (-not (Test-Path $inputRootResolved)) {
    throw "InputRoot does not exist: '$inputRootResolved'."
}
if (-not (Test-Path $certPathResolved)) {
    throw "CertPath does not exist: '$certPathResolved'."
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = Get-VersionString
}

$envVal = $env:AERO_MAKE_FAT_IMAGE
if ($null -eq $envVal) {
    $envVal = ""
}
$shouldMakeFatImage = $MakeFatImage -or (@("1", "true", "yes", "on") -contains $envVal.ToLowerInvariant())

New-Item -ItemType Directory -Force -Path $outDirResolved | Out-Null

$artifactBase = "AeroVirtIO-Win7-$Version"
$zipX86 = Join-Path $outDirResolved "$artifactBase-x86.zip"
$zipX64 = Join-Path $outDirResolved "$artifactBase-x64.zip"
$zipBundle = Join-Path $outDirResolved "$artifactBase-bundle.zip"
$isoBundle = Join-Path $outDirResolved "$artifactBase.iso"
$fatVhd = Join-Path $outDirResolved "$artifactBase-fat.vhd"

$stagingBase = Join-Path $outDirResolved "_staging"
if (Test-Path $stagingBase) {
    Remove-Item -Recurse -Force $stagingBase
}
New-Item -ItemType Directory -Force -Path $stagingBase | Out-Null

$stageX86 = Join-Path $stagingBase "x86"
$stageX64 = Join-Path $stagingBase "x64"
$stageBundle = Join-Path $stagingBase "bundle"
$stageFat = Join-Path $stagingBase "fat"

New-DriverArtifactRoot -DestRoot $stageX86 -CertSourcePath $certPathResolved
New-DriverArtifactRoot -DestRoot $stageX64 -CertSourcePath $certPathResolved
New-DriverArtifactRoot -DestRoot $stageBundle -CertSourcePath $certPathResolved

Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x86") -DestRoot $stageX86
Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x64") -DestRoot $stageX64
Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x86", "x64") -DestRoot $stageBundle

$success = $false
try {
    New-ZipFromFolder -Folder $stageX86 -ZipPath $zipX86
    New-ZipFromFolder -Folder $stageX64 -ZipPath $zipX64
    New-ZipFromFolder -Folder $stageBundle -ZipPath $zipBundle

    if (-not $NoIso) {
        $label = ("AEROVIRTIO_WIN7_" + $Version).ToUpperInvariant() -replace "[^A-Z0-9_]", "_"
        if ($label.Length -gt 32) {
            $label = $label.Substring(0, 32)
        }
        New-IsoFromFolder -Folder $stageBundle -IsoPath $isoBundle -VolumeLabel $label
    }

    if ($shouldMakeFatImage) {
        New-FatImageInputFromBundle -BundleRoot $stageBundle -CertSourcePath $certPathResolved -DestRoot $stageFat

        $helper = Join-Path $PSScriptRoot "make-fat-image.ps1"
        if (-not (Test-Path $helper)) {
            throw "Missing helper script: '$helper'."
        }

        & $helper -SourceDir $stageFat -OutFile $fatVhd -SizeMB $FatImageSizeMB -Strict:$FatImageStrict
    }
    $success = $true
} finally {
    if ($success -and (Test-Path $stagingBase)) {
        Remove-Item -Recurse -Force $stagingBase
    }
}

Write-Host "Artifacts created in '$outDirResolved':"
Write-Host "  $zipX86"
Write-Host "  $zipX64"
Write-Host "  $zipBundle"
if (-not $NoIso) {
    Write-Host "  $isoBundle"
}

if ($shouldMakeFatImage) {
    if (Test-Path $fatVhd) {
        Write-Host "  $fatVhd"
    } else {
        Write-Host "  (FAT image skipped)"
    }
}
