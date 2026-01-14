[CmdletBinding()]
param(
    [string] $SourcePath,
    [string] $IsoPath,
    [string] $VolumeLabel = "AEROVIRTIO",
    # Optional override for deterministic timestamps inside the ISO (seconds since Unix epoch).
    # Defaults to SOURCE_DATE_EPOCH if set, otherwise 0.
    [Nullable[long]] $SourceDateEpoch,
    # Force the legacy IMAPI2 implementation (Windows only). By default, when `cargo` is available,
    # this script uses the deterministic Rust ISO writer (`aero_iso`).
    [switch] $LegacyIso
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-IsWindows {
    try {
        return [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
    } catch {
        return $false
    }
}

function New-IsoFile {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string] $SourcePath,
        [Parameter(Mandatory = $true)][string] $IsoPath,
        [Parameter(Mandatory = $true)][string] $VolumeLabel,
        [Nullable[long]] $SourceDateEpoch,
        [switch] $LegacyIso
    )

    if (-not (Test-Path $SourcePath)) {
        throw "SourcePath does not exist: '$SourcePath'."
    }

    $outDir = Split-Path -Parent $IsoPath
    if (-not [string]::IsNullOrWhiteSpace($outDir)) {
        New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    }

    if (Test-Path $IsoPath) {
        Remove-Item -Force $IsoPath
    }

    $cargoExe = (Get-Command cargo -ErrorAction SilentlyContinue).Source
    $useRustIso = (-not $LegacyIso) -and -not [string]::IsNullOrWhiteSpace($cargoExe)
    if ($useRustIso) {
        $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\\..")).Path
        $manifestPath = Join-Path $repoRoot "tools\\packaging\\aero_packager\\Cargo.toml"
        if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            throw "Missing aero_packager Cargo.toml: '$manifestPath'."
        }

        $epoch = 0
        if ($null -ne $SourceDateEpoch) {
            $epoch = [int64] $SourceDateEpoch
        } elseif (-not [string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
            try {
                $epoch = [int64] $env:SOURCE_DATE_EPOCH
            } catch {
                throw "Invalid SOURCE_DATE_EPOCH (expected integer seconds): '$($env:SOURCE_DATE_EPOCH)'."
            }
        }

        & $cargoExe run --quiet --release --locked --manifest-path $manifestPath --bin aero_iso -- `
            --in-dir $SourcePath `
            --out-iso $IsoPath `
            --volume-id $VolumeLabel `
            --source-date-epoch $epoch
        if ($LASTEXITCODE -ne 0) {
            throw "Deterministic ISO creation failed (cargo exit code $LASTEXITCODE)."
        }
        return
    }

    if (-not (Get-IsWindows)) {
        throw "ISO creation requires either cargo (deterministic, cross-platform) or Windows IMAPI2. Install Rust/cargo, or run on Windows."
    }

    $fsi = New-Object -ComObject "IMAPI2FS.MsftFileSystemImage"
    $fsi.FileSystemsToCreate = 7
    $fsi.VolumeName = $VolumeLabel
    $fsi.Root.AddTree($SourcePath, $false)

    $result = $fsi.CreateResultImage()
    $stream = [System.Runtime.InteropServices.ComTypes.IStream] $result.ImageStream

    $stream.Seek(0, 0, [System.IntPtr]::Zero)
    $buffer = New-Object byte[] (1024 * 1024)
    $bytesRead = 0

    $file = [System.IO.File]::Open($IsoPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write)
    try {
        while ($true) {
            $stream.Read($buffer, $buffer.Length, [ref] $bytesRead)
            if ($bytesRead -le 0) {
                break
            }
            $file.Write($buffer, 0, $bytesRead)
        }
    } finally {
        $file.Dispose()
    }
}

if (-not [string]::IsNullOrWhiteSpace($SourcePath) -and -not [string]::IsNullOrWhiteSpace($IsoPath)) {
    New-IsoFile -SourcePath $SourcePath -IsoPath $IsoPath -VolumeLabel $VolumeLabel -SourceDateEpoch $SourceDateEpoch -LegacyIso:$LegacyIso
}

