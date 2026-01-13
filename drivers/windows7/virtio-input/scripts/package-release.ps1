# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
Packages the Aero virtio-input Windows 7 driver into a redistributable zip.

.DESCRIPTION
Collects the driver SYS built by WDK/VS, the INF tracked in this repo, and
optionally a signed CAT and KMDF coinstaller DLL, then emits:

  aero-virtio-input-win7-<arch>-<version>.zip

The zip always includes:
  - INSTALL.txt with minimal test-signing + "Have Disk..." install steps
  - manifest.json listing file hashes and metadata
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('x86', 'amd64', 'x64', 'both')]
    [string]$Arch,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$InputDir,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutDir,

    # When enabled, attempts to include the public test-signing certificate (.cer)
    # alongside the driver package to simplify manual installation on test machines.
    # The private key material (.pfx) is never included.
    [switch]$IncludeTestCert
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:DriverId = 'aero-virtio-input'
$script:InfBaseName = 'aero_virtio_input'
$script:ServiceNameFallback = $script:InfBaseName
$script:TargetOs = 'win7'
$script:FixedZipTimestamp = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:FallbackSysName = (($script:DriverId -replace '-', '_') + '.sys')
$script:InstallInstructionsName = 'INSTALL.txt'
$script:InstallInstructionsTemplateName = 'INSTALL.txt.in'
$script:TestCertFileName = 'aero-virtio-input-test.cer'

function Normalize-Arch([string]$ArchValue) {
    if ($ArchValue -eq 'x64') { return 'amd64' }
    return $ArchValue
}

function Format-PathList([string[]]$Paths) {
    return ($Paths | ForEach-Object { "  - $_" }) -join "`r`n"
}

function Resolve-ExistingDirectory([string]$Path, [string]$ArgName) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$ArgName does not exist: $Path"
    }
    $resolved = Resolve-Path -LiteralPath $Path
    if (-not (Test-Path -LiteralPath $resolved.Path -PathType Container)) {
        throw "$ArgName is not a directory: $Path"
    }
    return $resolved.Path
}

function Resolve-OrCreateDirectory([string]$Path, [string]$ArgName) {
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
    $resolved = Resolve-Path -LiteralPath $Path
    if (-not (Test-Path -LiteralPath $resolved.Path -PathType Container)) {
        throw "$ArgName is not a directory: $Path"
    }
    return $resolved.Path
}

function Get-ExpectedPeMachine([ValidateSet('x86', 'amd64')] [string]$ArchValue) {
    switch ($ArchValue) {
        'x86' { return 0x014c }  # IMAGE_FILE_MACHINE_I386
        'amd64' { return 0x8664 }  # IMAGE_FILE_MACHINE_AMD64
        default { throw "Unhandled arch '$ArchValue'." }
    }
}

function Get-PeMachine([string]$Path) {
    try {
        $fs = [System.IO.File]::OpenRead($Path)
        try {
            $br = New-Object System.IO.BinaryReader($fs)
            try {
                if ($br.ReadUInt16() -ne 0x5A4D) { return $null } # MZ
                $fs.Seek(0x3C, [System.IO.SeekOrigin]::Begin) | Out-Null
                $peOffset = $br.ReadUInt32()
                $fs.Seek([int64]$peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
                if ($br.ReadUInt32() -ne 0x00004550) { return $null } # PE\0\0
                return $br.ReadUInt16()
            }
            finally {
                $br.Dispose()
            }
        }
        finally {
            $fs.Dispose()
        }
    }
    catch {
        return $null
    }
}

function Find-PeFileForArch(
    [string]$RootDir,
    [string]$FileName,
    [ValidateSet('x86', 'amd64')] [string]$ArchValue,
    [switch]$Optional
) {
    $matches = @(
        Get-ChildItem -LiteralPath $RootDir -Recurse -File -Filter $FileName -ErrorAction SilentlyContinue
    )

    if ($matches.Count -eq 0) {
        if ($Optional) { return $null }
        throw "Could not find '$FileName' under -InputDir '$RootDir'."
    }

    $expectedMachine = Get-ExpectedPeMachine $ArchValue
    $archMatches = @()
    foreach ($m in $matches) {
        $machine = Get-PeMachine $m.FullName
        if ($machine -eq $expectedMachine) {
            $archMatches += $m
        }
    }

    if ($archMatches.Count -eq 1) {
        return $archMatches[0].FullName
    }

    if ($archMatches.Count -eq 0) {
        if ($Optional) { return $null }
        $candidatePaths = $matches | ForEach-Object { $_.FullName }
        throw ("Found '$FileName' but none match architecture '$ArchValue'. Candidates:`r`n{0}" -f (Format-PathList $candidatePaths))
    }

    $candidatePaths = $archMatches | ForEach-Object { $_.FullName }
    throw ("Found multiple copies of '$FileName' for architecture '$ArchValue'. Please clean old builds or point -InputDir at a single build output.`r`n{0}" -f (Format-PathList $candidatePaths))
}

function Find-OptionalFileByName(
    [string]$RootDir,
    [string]$FileName,
    [ValidateSet('x86', 'amd64')] [string]$ArchValue
) {
    $matches = @(
        Get-ChildItem -LiteralPath $RootDir -Recurse -File -Filter $FileName -ErrorAction SilentlyContinue
    )
    if ($matches.Count -eq 0) { return $null }
    if ($matches.Count -eq 1) { return $matches[0].FullName }

    if ($PSBoundParameters.ContainsKey('ArchValue')) {
        $scored = @()
        foreach ($m in $matches) {
            $p = $m.FullName.ToLowerInvariant()
            $score = 0
            if ($ArchValue -eq 'x86') {
                if ($p -match '[\\/](x86|i386|win32)[\\/]' ) { $score += 2 }
                if ($p -match '[\\/](x32|32)[\\/]' ) { $score += 1 }
            }
            else {
                if ($p -match '[\\/](amd64|x64|win64)[\\/]' ) { $score += 2 }
                if ($p -match '[\\/](x86_64|64)[\\/]' ) { $score += 1 }
            }
            $scored += [pscustomobject]@{ Path = $m.FullName; Score = $score }
        }

        $maxScore = ($scored | Measure-Object -Property Score -Maximum).Maximum
        $best = @($scored | Where-Object { $_.Score -eq $maxScore })
        if (($maxScore -gt 0) -and ($best.Count -eq 1)) {
            return $best[0].Path
        }
    }

    $candidatePaths = $matches | ForEach-Object { $_.FullName }
    throw ("Found multiple copies of '$FileName'. Please remove duplicates or point -InputDir at a single build output.`r`n{0}" -f (Format-PathList $candidatePaths))
}

function Find-TestCertPath(
    [string]$InputDirResolved,
    [string]$VirtioInputRootPath
) {
    $repoCertPath = Join-Path (Join-Path $VirtioInputRootPath 'cert') $script:TestCertFileName
    if (Test-Path -LiteralPath $repoCertPath -PathType Leaf) {
        return (Resolve-Path -LiteralPath $repoCertPath).Path
    }

    $matches = @(
        Get-ChildItem -LiteralPath $InputDirResolved -Recurse -File -Filter '*.cer' -ErrorAction SilentlyContinue
    )
    if ($matches.Count -eq 0) { return $null }

    $exact = @($matches | Where-Object { $_.Name -ieq $script:TestCertFileName })
    if ($exact.Count -eq 1) { return $exact[0].FullName }
    if ($exact.Count -gt 1) {
        $candidatePaths = $exact | ForEach-Object { $_.FullName }
        throw ("Found multiple copies of '{0}' under -InputDir '{1}'. Please remove duplicates or point -InputDir at a single build output.`r`n{2}" -f $script:TestCertFileName, $InputDirResolved, (Format-PathList $candidatePaths))
    }

    if ($matches.Count -eq 1) { return $matches[0].FullName }

    $candidatePaths = $matches | ForEach-Object { $_.FullName }
    throw ("Found multiple .cer files under -InputDir '{0}'. Refusing to guess which one to include. Candidates:`r`n{1}" -f $InputDirResolved, (Format-PathList $candidatePaths))
}

function Get-InfPathForArch([string]$InfDir, [ValidateSet('x86', 'amd64')] [string]$ArchValue) {
    $archSpecific = Join-Path $InfDir ("{0}-{1}.inf" -f $script:InfBaseName, $ArchValue)
    if (Test-Path -LiteralPath $archSpecific) { return $archSpecific }

    $unified = Join-Path $InfDir "$script:InfBaseName.inf"
    if (Test-Path -LiteralPath $unified) { return $unified }

    throw ("INF not found. Expected either:`r`n  - {0}`r`n  - {1}" -f $unified, $archSpecific)
}

function Get-DriverVerFromInf([string]$InfPath) {
    $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            '^\s*DriverVer\s*=\s*([^,]+)\s*,\s*([^;\s]+)',
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) {
            return [ordered]@{
                date = $m.Groups[1].Value.Trim()
                version = $m.Groups[2].Value.Trim()
            }
        }
    }
    throw "Could not find a DriverVer=...,... line in INF: $InfPath"
}

function Get-SysFileNameFromInf([string]$InfPath, [string]$DefaultName) {
    $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
    $names = @()
    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            '^\s*ServiceBinary\s*=\s*.*\\\s*([^\s\\;]+\.sys)\b',
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) { $names += $m.Groups[1].Value.Trim() }
    }

    $names = @($names | Select-Object -Unique)
    if ($names.Count -eq 1) { return $names[0] }
    if ($names.Count -gt 1) {
        throw ("INF contains multiple distinct ServiceBinary SYS names: {0}" -f ($names -join ', '))
    }
    return $DefaultName
}

function Get-CatalogFileNameFromInf([string]$InfPath, [ValidateSet('x86', 'amd64')] [string]$ArchValue) {
    $archTag = if ($ArchValue -eq 'x86') { 'NTx86' } else { 'NTamd64' }
    $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop

    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            ('^\s*CatalogFile\.{0}\s*=\s*([^\s;]+\.cat)\b' -f [regex]::Escape($archTag)),
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) { return $m.Groups[1].Value.Trim() }
    }

    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            '^\s*CatalogFile(\.NT)?\s*=\s*([^\s;]+\.cat)\b',
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) { return $m.Groups[2].Value.Trim() }
    }

    return $null
}

function Get-WdfCoInstallerDllNameFromInf([string]$InfPath) {
    $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
    $names = @()
    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            '(WdfCoInstaller[0-9A-Za-z]+\.dll)',
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) { $names += $m.Groups[1].Value }
    }

    $names = @($names | Select-Object -Unique)
    if ($names.Count -eq 1) { return $names[0] }
    if ($names.Count -gt 1) {
        throw ("INF references multiple distinct WDF coinstaller DLL names: {0}" -f ($names -join ', '))
    }
    return $null
}

function Get-ServiceNameFromInf([string]$InfPath, [string]$DefaultName) {
    $lines = Get-Content -LiteralPath $InfPath -ErrorAction Stop
    $names = @()
    foreach ($line in $lines) {
        $m = [regex]::Match(
            $line,
            '^\s*AddService\s*=\s*([^,\s]+)\s*,',
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
        )
        if ($m.Success) { $names += $m.Groups[1].Value.Trim() }
    }

    $names = @($names | Select-Object -Unique)
    if ($names.Count -eq 1) { return $names[0] }
    if ($names.Count -gt 1) {
        throw ("INF contains multiple distinct AddService service names: {0}" -f ($names -join ', '))
    }
    return $DefaultName
}

function New-DeterministicZip([string]$SourceDir, [string]$ZipPath) {
    Add-Type -AssemblyName System.IO.Compression | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

    if (Test-Path -LiteralPath $ZipPath) {
        Remove-Item -LiteralPath $ZipPath -Force
    }

    $files = @(
        Get-ChildItem -LiteralPath $SourceDir -File | Sort-Object -Property Name
    )

    $zipStream = [System.IO.File]::Open($ZipPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
    try {
        $zip = New-Object System.IO.Compression.ZipArchive(
            $zipStream,
            [System.IO.Compression.ZipArchiveMode]::Create,
            $false
        )
        try {
            foreach ($f in $files) {
                $entry = $zip.CreateEntry($f.Name, [System.IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime = $script:FixedZipTimestamp
                $entryStream = $entry.Open()
                try {
                    $fileStream = [System.IO.File]::OpenRead($f.FullName)
                    try {
                        $fileStream.CopyTo($entryStream)
                    }
                    finally {
                        $fileStream.Dispose()
                    }
                }
                finally {
                    $entryStream.Dispose()
                }
            }
        }
        finally {
            $zip.Dispose()
        }
    }
    finally {
        $zipStream.Dispose()
    }
}

function Write-Utf8NoBomFile([string]$Path, [string]$Contents) {
    [System.IO.File]::WriteAllText($Path, $Contents, $script:Utf8NoBom)
}

function Write-Sha256SumsFile([string]$DirPath) {
    $files = @(
        Get-ChildItem -LiteralPath $DirPath -File |
            Where-Object { $_.Name -ne 'SHA256SUMS' } |
            Sort-Object -Property Name
    )

    $lines = @()
    foreach ($f in $files) {
        $hash = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $lines += ("{0}  {1}" -f $hash, $f.Name)
    }

    # Use LF newlines for compatibility with `sha256sum -c` on non-Windows hosts.
    $contents = (($lines -join "`n") + "`n")
    Write-Utf8NoBomFile -Path (Join-Path $DirPath 'SHA256SUMS') -Contents $contents
}

function Get-NormalizedCrlfText([string]$Text) {
    $t = $Text -replace "`r`n", "`n"
    $t = $t -replace "`r", "`n"
    return ($t -replace "`n", "`r`n")
}

function New-InstallInstructionsText(
    [string]$TemplatePath,
    [ValidateSet('x86', 'amd64')] [string]$ArchValue,
    [string]$Version,
    [string]$InfLeaf,
    [string]$SysName,
    [string]$ServiceName
 ) {
    $template = Get-Content -LiteralPath $TemplatePath -Raw -ErrorAction Stop
    $template = Get-NormalizedCrlfText $template

    $text = $template
    $text = $text.Replace('{{ARCH}}', $ArchValue)
    $text = $text.Replace('{{VERSION}}', $Version)
    $text = $text.Replace('{{INF}}', $InfLeaf)
    $text = $text.Replace('{{SYS}}', $SysName)
    $text = $text.Replace('{{SERVICE}}', $ServiceName)
    return $text
}

function Package-OneArch(
    [ValidateSet('x86', 'amd64')] [string]$ArchValue,
    [string]$InputDirResolved,
    [string]$InfDirResolved,
    [string]$OutDirResolved,
    [string]$TestCertPath,
    [ref]$SharedVersion
) {
    $infPath = Get-InfPathForArch -InfDir $InfDirResolved -ArchValue $ArchValue
    $driverVer = Get-DriverVerFromInf -InfPath $infPath

    if (($null -ne $SharedVersion.Value) -and ($SharedVersion.Value -ne $driverVer.version)) {
        throw ("Version mismatch between packages. Previous arch used version '{0}', but INF '{1}' reports '{2}'." -f $SharedVersion.Value, $infPath, $driverVer.version)
    }
    $SharedVersion.Value = $driverVer.version

    $serviceName = Get-ServiceNameFromInf -InfPath $infPath -DefaultName $script:ServiceNameFallback

    $sysName = Get-SysFileNameFromInf -InfPath $infPath -DefaultName $script:FallbackSysName
    $sysPath = Find-PeFileForArch -RootDir $InputDirResolved -FileName $sysName -ArchValue $ArchValue

    $catName = Get-CatalogFileNameFromInf -InfPath $infPath -ArchValue $ArchValue
    $catPath = $null
    if ($null -ne $catName) {
        $catSibling = Join-Path (Split-Path -Parent $infPath) $catName
        if (Test-Path -LiteralPath $catSibling) {
            $catPath = $catSibling
        }
        else {
            $catPath = Find-OptionalFileByName -RootDir $InputDirResolved -FileName $catName -ArchValue $ArchValue
            if ($null -eq $catPath) {
                Write-Warning ("Catalog file '{0}' was referenced by INF but not found; continuing without it." -f $catName)
            }
        }
    }

    $coInstallerName = Get-WdfCoInstallerDllNameFromInf -InfPath $infPath
    $coInstallerPath = $null
    if ($null -ne $coInstallerName) {
        $coInstallerPath = Find-PeFileForArch -RootDir $InputDirResolved -FileName $coInstallerName -ArchValue $ArchValue -Optional
        if ($null -eq $coInstallerPath) {
            Write-Warning ("KMDF coinstaller '{0}' was referenced by INF but not found; continuing without it." -f $coInstallerName)
        }
    }
    else {
        $coInstallerPath = Find-PeFileForArch -RootDir $InputDirResolved -FileName 'WdfCoInstaller*.dll' -ArchValue $ArchValue -Optional
    }

    $stageDir = Join-Path ([System.IO.Path]::GetTempPath()) ("{0}-{1}-{2}-{3}" -f $script:DriverId, $script:TargetOs, $ArchValue, [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

    try {
        $infLeaf = Split-Path -Leaf $infPath
        Copy-Item -LiteralPath $infPath -Destination (Join-Path $stageDir $infLeaf) -Force
        Copy-Item -LiteralPath $sysPath -Destination (Join-Path $stageDir $sysName) -Force

        if ($null -ne $catPath) {
            Copy-Item -LiteralPath $catPath -Destination (Join-Path $stageDir (Split-Path -Leaf $catPath)) -Force
        }
        if ($null -ne $coInstallerPath) {
            Copy-Item -LiteralPath $coInstallerPath -Destination (Join-Path $stageDir (Split-Path -Leaf $coInstallerPath)) -Force
        }
        if ($null -ne $TestCertPath) {
            Copy-Item -LiteralPath $TestCertPath -Destination (Join-Path $stageDir $script:TestCertFileName) -Force
        }

        $installPath = Join-Path $stageDir $script:InstallInstructionsName
        $installText = New-InstallInstructionsText `
            -TemplatePath $script:InstallInstructionsTemplatePath `
            -ArchValue $ArchValue `
            -Version $driverVer.version `
            -InfLeaf $infLeaf `
            -SysName $sysName `
            -ServiceName $serviceName
        Write-Utf8NoBomFile -Path $installPath -Contents $installText

        $payloadFiles = @(
            Get-ChildItem -LiteralPath $stageDir -File | Sort-Object -Property Name
        )

        $filesManifest = @()
        foreach ($f in $payloadFiles) {
            $hash = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            $filesManifest += [ordered]@{
                path = $f.Name
                sha256 = $hash
                size = [int64]$f.Length
            }
        }

        $manifest = [ordered]@{
            schemaVersion = 1
            driver = [ordered]@{
                id = $script:DriverId
                targetOs = $script:TargetOs
                arch = $ArchValue
                version = $driverVer.version
                driverVerDate = $driverVer.date
                inf = $infLeaf
                sys = $sysName
                cat = if ($null -ne $catPath) { Split-Path -Leaf $catPath } else { $null }
                kmdfCoInstaller = if ($null -ne $coInstallerPath) { Split-Path -Leaf $coInstallerPath } else { $null }
            }
            files = $filesManifest
        }

        $manifestPath = Join-Path $stageDir 'manifest.json'
        Write-Utf8NoBomFile -Path $manifestPath -Contents ($manifest | ConvertTo-Json -Depth 10 -Compress)

        Write-Sha256SumsFile -DirPath $stageDir

        $zipName = ("{0}-{1}-{2}-{3}.zip" -f $script:DriverId, $script:TargetOs, $ArchValue, $driverVer.version)
        $zipPath = Join-Path $OutDirResolved $zipName
        New-DeterministicZip -SourceDir $stageDir -ZipPath $zipPath

        Write-Host ("Created {0}" -f $zipPath)
    }
    finally {
        if (Test-Path -LiteralPath $stageDir) {
            Remove-Item -LiteralPath $stageDir -Recurse -Force
        }
    }
}

$inputDirResolved = Resolve-ExistingDirectory -Path $InputDir -ArgName '-InputDir'
$outDirResolved = Resolve-OrCreateDirectory -Path $OutDir -ArgName '-OutDir'

$virtioInputRoot = Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')
$infDirResolved = Join-Path $virtioInputRoot.Path 'inf'
if (-not (Test-Path -LiteralPath $infDirResolved -PathType Container)) {
    throw "INF directory not found: $infDirResolved"
}

$releaseDirResolved = Join-Path $virtioInputRoot.Path 'release'
$script:InstallInstructionsTemplatePath = Join-Path $releaseDirResolved $script:InstallInstructionsTemplateName
if (-not (Test-Path -LiteralPath $script:InstallInstructionsTemplatePath -PathType Leaf)) {
    throw ("Install instructions template not found: {0}" -f $script:InstallInstructionsTemplatePath)
}

$archList = if ($Arch -eq 'both') { @('x86', 'amd64') } else { @(Normalize-Arch $Arch) }
$sharedVersion = $null
$testCertPath = $null

if ($IncludeTestCert) {
    $testCertPath = Find-TestCertPath -InputDirResolved $inputDirResolved -VirtioInputRootPath $virtioInputRoot.Path
    if ($null -eq $testCertPath) {
        $expectedRepoPath = Join-Path (Join-Path $virtioInputRoot.Path 'cert') $script:TestCertFileName
        throw ("-IncludeTestCert was specified, but no test certificate was found. Expected either:`r`n  - {0}`r`n  - any *.cer under -InputDir '{1}'" -f $expectedRepoPath, $inputDirResolved)
    }
}

foreach ($a in $archList) {
    Package-OneArch -ArchValue $a -InputDirResolved $inputDirResolved -InfDirResolved $infDirResolved -OutDirResolved $outDirResolved -TestCertPath $testCertPath -SharedVersion ([ref]$sharedVersion)
}
