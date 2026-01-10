[CmdletBinding()]
param(
    [string] $SourcePath,
    [string] $IsoPath,
    [string] $VolumeLabel = "AEROVIRTIO"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function New-IsoFile {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string] $SourcePath,
        [Parameter(Mandatory = $true)][string] $IsoPath,
        [Parameter(Mandatory = $true)][string] $VolumeLabel
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
    New-IsoFile -SourcePath $SourcePath -IsoPath $IsoPath -VolumeLabel $VolumeLabel
}

