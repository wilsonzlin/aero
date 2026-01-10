[CmdletBinding()]
param(
    # Directory containing the signed driver package root.
    # Expected layout:
    #   aero-test.cer
    #   INSTALL.txt
    #   x86/...
    #   x64/...
    [Parameter(Mandatory = $true)]
    [string]$SourceDir,

    # Destination FAT disk image. VHD is used because it's createable with built-in
    # Windows tools (DiskPart) without external dependencies.
    [string]$OutFile = "out/artifacts/aero-drivers-fat.vhd",

    # Desired maximum size of the VHD (in MB). If too small for the contents of
    # $SourceDir, the script automatically bumps the size to fit.
    [int]$SizeMB = 64,

    # If set, failing to create/mount/format the image is treated as an error.
    # Otherwise we emit a warning and skip FAT image creation.
    [switch]$Strict
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Test-IsWindowsPlatform {
    return ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT)
}

$isWindows = Test-IsWindowsPlatform

function Test-IsAdministrator {
    if (-not $isWindows) {
        return $false
    }

    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-FreeDriveLetter {
    $used = @(Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Name.ToUpperInvariant() })

    foreach ($c in [char[]]([char]'Z'..[char]'D')) {
        $letter = $c.ToString()
        if ($used -notcontains $letter) {
            return $letter
        }
    }

    return $null
}

function Invoke-DiskPart {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Script
    )

    $scriptPath = Join-Path ([IO.Path]::GetTempPath()) ("aero-diskpart-{0}.txt" -f ([Guid]::NewGuid().ToString("N")))
    try {
        # DiskPart historically expects ASCII/ANSI script files.
        Set-Content -Path $scriptPath -Value $Script -Encoding ASCII
        $output = & diskpart /s $scriptPath 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) {
            throw "DiskPart exited with code $LASTEXITCODE.`n$output"
        }
        if ($output -match "DiskPart has encountered an error" -or $output -match "Access is denied") {
            throw "DiskPart reported an error.`n$output"
        }
        return $output
    } finally {
        Remove-Item -LiteralPath $scriptPath -Force -ErrorAction SilentlyContinue
    }
}

function Skip-OrThrow {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    if ($Strict) {
        throw $Message
    }
    Write-Warning $Message
    return $false
}

$sourceDirPath = (Resolve-Path -LiteralPath $SourceDir).Path
if (-not (Test-Path -LiteralPath $sourceDirPath -PathType Container)) {
    throw "SourceDir '$SourceDir' does not exist or is not a directory."
}

foreach ($rel in @("aero-test.cer", "INSTALL.txt", "x86", "x64")) {
    $p = Join-Path $sourceDirPath $rel
    if (-not (Test-Path -LiteralPath $p)) {
        throw "SourceDir is missing required path '$rel' ($p)."
    }
}

$sourceBytes = (Get-ChildItem -LiteralPath $sourceDirPath -Recurse -File | Measure-Object -Property Length -Sum).Sum
if ($null -eq $sourceBytes) {
    $sourceBytes = 0
}

# FAT32 + directory overhead is small, but give generous slack to avoid copy failures.
$minSizeMB = [math]::Ceiling((($sourceBytes * 1.2) + 8MB) / 1MB)
$requestedSizeMB = [math]::Max($SizeMB, [math]::Max(64, $minSizeMB))
if ($requestedSizeMB -ne $SizeMB) {
    Write-Host ("[make-fat-image] Bumping FAT image size from {0}MB -> {1}MB (input is ~{2}MB)" -f $SizeMB, $requestedSizeMB, ([math]::Ceiling($sourceBytes / 1MB)))
    $SizeMB = $requestedSizeMB
}

$outFilePath = [IO.Path]::GetFullPath($OutFile)
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outFilePath) | Out-Null
if (Test-Path -LiteralPath $outFilePath) {
    Remove-Item -LiteralPath $outFilePath -Force
}

if (-not $isWindows) {
    if (-not (Skip-OrThrow "Skipping FAT image creation: this script requires Windows (DiskPart + VHD mounting).")) {
        return
    }
}

if (-not (Test-IsAdministrator)) {
    if (-not (Skip-OrThrow "Skipping FAT image creation: administrator privileges are required to create/mount/format VHDs.")) {
        return
    }
}

$driveLetter = Get-FreeDriveLetter
if ($null -eq $driveLetter) {
    if (-not (Skip-OrThrow "Skipping FAT image creation: unable to find a free drive letter.")) {
        return
    }
}

$destRoot = "{0}:\\" -f $driveLetter
$attached = $false
$cleanupOutFile = $false

try {
    Write-Host ("[make-fat-image] Creating {0}MB FAT32 VHD at {1}" -f $SizeMB, $outFilePath)

    $createScript = @"
create vdisk file="$outFilePath" maximum=$SizeMB type=expandable
select vdisk file="$outFilePath"
attach vdisk
create partition primary
format fs=fat32 quick label=AERO
assign letter=$driveLetter
exit
"@

    Invoke-DiskPart -Script $createScript | Out-Null
    $attached = $true

    for ($i = 0; $i -lt 50; $i++) {
        if (Test-Path -LiteralPath $destRoot) {
            break
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not (Test-Path -LiteralPath $destRoot)) {
        throw "Mounted VHD did not appear as drive $destRoot"
    }

    Write-Host ("[make-fat-image] Copying files from {0} -> {1}" -f $sourceDirPath, $destRoot)
    $robocopyCmd = Get-Command robocopy -ErrorAction SilentlyContinue
    if ($null -ne $robocopyCmd) {
        & robocopy $sourceDirPath $destRoot /MIR /R:3 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        # Robocopy exit codes are a bitfield; anything >= 8 indicates a failure.
        if ($LASTEXITCODE -ge 8) {
            throw "Robocopy failed with exit code $LASTEXITCODE."
        }
    } else {
        Copy-Item -Path (Join-Path $sourceDirPath "*") -Destination $destRoot -Recurse -Force
    }

    foreach ($rel in @("aero-test.cer", "INSTALL.txt", "x86", "x64")) {
        $p = Join-Path $destRoot $rel
        if (-not (Test-Path -LiteralPath $p)) {
            throw "FAT image verification failed: missing '$rel' in mounted image ($p)."
        }
    }

    Write-Host "[make-fat-image] FAT image contents verified."
} catch {
    $message = $_.Exception.Message
    if (-not (Skip-OrThrow ("FAT image creation failed: {0}" -f $message))) {
        $cleanupOutFile = $true
        return
    }
    throw
} finally {
    if ($attached) {
        try {
            $detachScript = @"
select vdisk file="$outFilePath"
detach vdisk
exit
"@
            Invoke-DiskPart -Script $detachScript | Out-Null
        } catch {
            Write-Warning ("[make-fat-image] Failed to detach VHD (manual cleanup may be required): {0}" -f $_.Exception.Message)
        }
    }

    if ($cleanupOutFile) {
        Remove-Item -LiteralPath $outFilePath -Force -ErrorAction SilentlyContinue
    }
}

$img = Get-Item -LiteralPath $outFilePath
if ($img.Length -le 0) {
    throw "Created FAT image is empty: $outFilePath"
}

Write-Host ("[make-fat-image] Wrote {0} ({1} bytes)" -f $outFilePath, $img.Length)
