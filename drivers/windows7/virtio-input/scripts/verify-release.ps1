# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
Validates an Aero virtio-input Win7 driver release zip against its manifest.json.

.DESCRIPTION
Extracts the zip to a temporary directory, parses manifest.json, and for each
entry in manifest.files verifies:

  - File exists
  - File size matches
  - SHA-256 hash matches

If a `SHA256SUMS` file is present, it is also validated:
  - Every listed file exists and its hash matches
  - `manifest.json` is covered
  - Every extracted file (except `SHA256SUMS` itself) is listed

Also validates basic identity fields:
  - schemaVersion == 1
  - driver.id == aero-virtio-input
  - driver.targetOs == win7

Fails with a non-zero exit code and a clear error message on any mismatch.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ZipPath
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:ExpectedSchemaVersion = 1
$script:ExpectedDriverId = 'aero-virtio-input'
$script:ExpectedTargetOs = 'win7'

function Resolve-ExistingFile([string]$Path, [string]$ArgName) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$ArgName does not exist: $Path"
    }
    $resolved = Resolve-Path -LiteralPath $Path
    if (-not (Test-Path -LiteralPath $resolved.Path -PathType Leaf)) {
        throw "$ArgName is not a file: $Path"
    }
    return $resolved.Path
}

function Get-FullPathSafe([string]$Path) {
    # GetFullPath normalizes ".." segments which we rely on for containment checks.
    return [System.IO.Path]::GetFullPath($Path)
}

function Assert-PathIsWithinDirectory([string]$RootDir, [string]$CandidatePath, [string]$Context) {
    $rootFull = Get-FullPathSafe $RootDir
    $candidateFull = Get-FullPathSafe $CandidatePath

    $sep = [System.IO.Path]::DirectorySeparatorChar
    if (-not $rootFull.EndsWith($sep)) {
        $rootFull = $rootFull + $sep
    }

    # Windows paths are typically case-insensitive; other platforms are not. Use
    # a comparison mode that matches the filesystem behavior so containment checks
    # remain correct when running under PowerShell Core on non-Windows hosts.
    $comparison = if ($env:OS -eq 'Windows_NT') {
        [System.StringComparison]::OrdinalIgnoreCase
    }
    else {
        [System.StringComparison]::Ordinal
    }

    if (-not $candidateFull.StartsWith($rootFull, $comparison)) {
        throw ("{0} resolves outside extraction directory. Root='{1}' Candidate='{2}'" -f $Context, $rootFull, $candidateFull)
    }
}

function New-TempDirectory([string]$Prefix) {
    $base = [System.IO.Path]::GetTempPath()
    $dir = Join-Path $base ("{0}{1}" -f $Prefix, [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    return $dir
}

function Expand-ZipSafe([string]$ZipFilePath, [string]$DestinationDir) {
    Add-Type -AssemblyName System.IO.Compression | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

    $zip = [System.IO.Compression.ZipFile]::OpenRead($ZipFilePath)
    try {
        foreach ($entry in $zip.Entries) {
            # Skip directory entries.
            if ([string]::IsNullOrEmpty($entry.Name)) {
                continue
            }

            $name = $entry.FullName

            if ($name.StartsWith('/') -or $name.StartsWith('\\') -or ($name -match '^[A-Za-z]:')) {
                throw ("Zip contains an absolute path entry: '{0}'" -f $name)
            }

            $destPath = Join-Path $DestinationDir $name
            Assert-PathIsWithinDirectory -RootDir $DestinationDir -CandidatePath $destPath -Context ("Zip entry '{0}'" -f $name)

            $destDir = Split-Path -Parent $destPath
            if (-not (Test-Path -LiteralPath $destDir -PathType Container)) {
                New-Item -ItemType Directory -Path $destDir -Force | Out-Null
            }

            if (Test-Path -LiteralPath $destPath) {
                throw ("Zip contains duplicate entry: '{0}'" -f $name)
            }

            $inStream = $entry.Open()
            try {
                $outStream = [System.IO.File]::Open($destPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
                try {
                    $inStream.CopyTo($outStream)
                }
                finally {
                    $outStream.Dispose()
                }
            }
            finally {
                $inStream.Dispose()
            }
        }
    }
    finally {
        $zip.Dispose()
    }
}

function Load-Manifest([string]$ExtractDir) {
    $manifestPath = Join-Path $ExtractDir 'manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "manifest.json not found in zip."
    }

    $raw = Get-Content -LiteralPath $manifestPath -Raw -ErrorAction Stop
    try {
        return $raw | ConvertFrom-Json -ErrorAction Stop
    }
    catch {
        throw ("manifest.json is not valid JSON: {0}" -f $_.Exception.Message)
    }
}

function Require-ManifestField($Value, [string]$Path) {
    if ($null -eq $Value) {
        throw ("manifest.json missing required field: {0}" -f $Path)
    }
    return $Value
}

function Assert-ManifestIdentity($Manifest) {
    $schema = Require-ManifestField $Manifest.schemaVersion 'schemaVersion'
    if ($schema -ne $script:ExpectedSchemaVersion) {
        throw ("Unsupported manifest schemaVersion. Expected {0}, got {1}." -f $script:ExpectedSchemaVersion, $schema)
    }

    $driver = Require-ManifestField $Manifest.driver 'driver'
    $id = Require-ManifestField $driver.id 'driver.id'
    $targetOs = Require-ManifestField $driver.targetOs 'driver.targetOs'

    if ($id -ne $script:ExpectedDriverId) {
        throw ("Unexpected driver.id. Expected '{0}', got '{1}'." -f $script:ExpectedDriverId, $id)
    }
    if ($targetOs -ne $script:ExpectedTargetOs) {
        throw ("Unexpected driver.targetOs. Expected '{0}', got '{1}'." -f $script:ExpectedTargetOs, $targetOs)
    }
}

function Assert-FilesMatchManifest($Manifest, [string]$ExtractDir) {
    $files = Require-ManifestField $Manifest.files 'files'

    $i = 0
    foreach ($f in $files) {
        $path = Require-ManifestField $f.path ("files[{0}].path" -f $i)
        $expectedHash = Require-ManifestField $f.sha256 ("files[{0}].sha256" -f $i)
        $expectedSize = Require-ManifestField $f.size ("files[{0}].size" -f $i)

        if ([string]::IsNullOrWhiteSpace($path)) {
            throw ("files[{0}].path must not be empty." -f $i)
        }

        $candidatePath = Join-Path $ExtractDir $path
        Assert-PathIsWithinDirectory -RootDir $ExtractDir -CandidatePath $candidatePath -Context ("Manifest path '{0}'" -f $path)

        if (-not (Test-Path -LiteralPath $candidatePath -PathType Leaf)) {
            throw ("Missing file listed in manifest: '{0}'" -f $path)
        }

        $item = Get-Item -LiteralPath $candidatePath -ErrorAction Stop
        $actualSize = [int64]$item.Length
        $expectedSize64 = [int64]$expectedSize

        if ($actualSize -ne $expectedSize64) {
            throw ("Size mismatch for '{0}': expected {1}, got {2}." -f $path, $expectedSize64, $actualSize)
        }

        $actualHash = (Get-FileHash -LiteralPath $candidatePath -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
        $expectedHashNorm = $expectedHash.ToString().ToLowerInvariant()

        if ($actualHash -ne $expectedHashNorm) {
            throw ("SHA256 mismatch for '{0}': expected {1}, got {2}." -f $path, $expectedHashNorm, $actualHash)
        }

        $i += 1
    }
}

$script:Sha256SumsFileName = 'SHA256SUMS'

function Assert-Sha256SumsFile([string]$ExtractDir) {
    $sumsPath = Join-Path $ExtractDir $script:Sha256SumsFileName
    if (-not (Test-Path -LiteralPath $sumsPath -PathType Leaf)) {
        Write-Warning ("{0} not found in zip; skipping SHA256SUMS validation." -f $script:Sha256SumsFileName)
        return
    }

    $comparison = if ($env:OS -eq 'Windows_NT') {
        [System.StringComparer]::OrdinalIgnoreCase
    }
    else {
        [System.StringComparer]::Ordinal
    }

    $seen = New-Object 'System.Collections.Generic.Dictionary[string,string]' $comparison

    $raw = Get-Content -LiteralPath $sumsPath -Raw -ErrorAction Stop
    $lines = $raw -split "`n"
    $lineNo = 0
    foreach ($line in $lines) {
        $lineNo += 1
        $trimmed = $line.TrimEnd("`r")
        if ([string]::IsNullOrWhiteSpace($trimmed)) {
            continue
        }

        $m = [regex]::Match($trimmed, '^\s*([0-9a-fA-F]{64})\s\s(.+?)\s*$')
        if (-not $m.Success) {
            throw ("Invalid {0} line {1}: '{2}'" -f $script:Sha256SumsFileName, $lineNo, $trimmed)
        }

        $expectedHash = $m.Groups[1].Value.ToLowerInvariant()
        $fileName = $m.Groups[2].Value

        if ($fileName -match '[\\/]') {
            throw ("Invalid filename in {0} (line {1}): '{2}' (paths are not allowed)" -f $script:Sha256SumsFileName, $lineNo, $fileName)
        }
        if ($fileName -ieq $script:Sha256SumsFileName) {
            throw ("{0} must not list itself (line {1})." -f $script:Sha256SumsFileName, $lineNo)
        }

        if ($seen.ContainsKey($fileName)) {
            throw ("Duplicate {0} entry for '{1}' (line {2})." -f $script:Sha256SumsFileName, $fileName, $lineNo)
        }
        $seen[$fileName] = $expectedHash

        $candidatePath = Join-Path $ExtractDir $fileName
        Assert-PathIsWithinDirectory -RootDir $ExtractDir -CandidatePath $candidatePath -Context ("{0} entry '{1}'" -f $script:Sha256SumsFileName, $fileName)

        if (-not (Test-Path -LiteralPath $candidatePath -PathType Leaf)) {
            throw ("Missing file referenced by {0}: '{1}'" -f $script:Sha256SumsFileName, $fileName)
        }

        $actualHash = (Get-FileHash -LiteralPath $candidatePath -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant()
        if ($actualHash -ne $expectedHash) {
            throw ("SHA256SUMS mismatch for '{0}': expected {1}, got {2}." -f $fileName, $expectedHash, $actualHash)
        }
    }

    if (-not $seen.ContainsKey('manifest.json')) {
        throw ("{0} is missing an entry for manifest.json." -f $script:Sha256SumsFileName)
    }

    $allFiles = @(
        Get-ChildItem -LiteralPath $ExtractDir -File -ErrorAction Stop |
            Where-Object { $_.Name -ne $script:Sha256SumsFileName }
    )
    $missing = @()
    foreach ($f in $allFiles) {
        if (-not $seen.ContainsKey($f.Name)) {
            $missing += $f.Name
        }
    }
    if ($missing.Count -gt 0) {
        $list = ($missing | Sort-Object | ForEach-Object { "  - $_" }) -join "`r`n"
        throw ("{0} is missing entries for one or more files:`r`n{1}" -f $script:Sha256SumsFileName, $list)
    }
}

$exitCode = 0
$tempDir = $null

try {
    $zipResolved = Resolve-ExistingFile -Path $ZipPath -ArgName '-ZipPath'
    $tempDir = New-TempDirectory -Prefix 'aero-virtio-input-verify-'

    Expand-ZipSafe -ZipFilePath $zipResolved -DestinationDir $tempDir

    $manifest = Load-Manifest -ExtractDir $tempDir
    Assert-ManifestIdentity -Manifest $manifest
    Assert-FilesMatchManifest -Manifest $manifest -ExtractDir $tempDir
    Assert-Sha256SumsFile -ExtractDir $tempDir

    Write-Host "OK: release zip matches manifest.json"
}
catch {
    $exitCode = 1
    Write-Error $_.Exception.Message
}
finally {
    if (($null -ne $tempDir) -and (Test-Path -LiteralPath $tempDir)) {
        Remove-Item -LiteralPath $tempDir -Recurse -Force
    }
}

exit $exitCode
