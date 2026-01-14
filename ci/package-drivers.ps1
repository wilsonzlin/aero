[CmdletBinding()]
param(
    [string] $InputRoot = "out/packages",

    # Driver signing / boot policy for the packaged media.
    #
    # - test: media is intended for test-signed/custom-signed drivers (default)
    # - production: media is intended for WHQL/production-signed drivers (no cert injection)
    # - none: same as production (development use)
    #
    # Legacy aliases accepted:
    # - testsigning / test-signing -> test
    # - nointegritychecks / no-integrity-checks -> none
    # - prod / whql -> production
    [ValidateSet("test", "production", "none", "testsigning", "test-signing", "nointegritychecks", "no-integrity-checks", "prod", "whql")]
    [string] $SigningPolicy = "test",

    [string] $CertPath = "out/certs/aero-test.cer",
    [string] $OutDir = "out/artifacts",
    # By default, the script only allows OutDir under <repo>/out to avoid accidental deletion of
    # arbitrary directories (it deletes $OutDir/_staging). Use -AllowUnsafeOutDir to override.
    [switch] $AllowUnsafeOutDir,
    [string] $Version,
    [string] $DriverNameMapJson,
    [switch] $SelfTest,
    [switch] $NoIso,
    [switch] $LegacyIso,
    [switch] $MakeFatImage,
    [switch] $FatImageStrict,
    [int] $FatImageSizeMB = 64,
    # Disable integrity manifest generation for the produced artifacts.
    # By default, manifests are written alongside each ZIP/ISO (if produced).
    [switch] $NoManifest,
    [switch] $DeterminismSelfTest
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

# Optional driver rename overrides loaded from -DriverNameMapJson.
# Keys may be either:
# - a driverRel (relative path under out/packages, e.g. "windows7/virtio-blk"), or
# - a leaf driver folder name (e.g. "blk")
$script:DriverNameMap = @{}

function Resolve-RepoPath {
    param([Parameter(Mandatory = $true)][string] $Path)

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }

    $repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
    return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

function Assert-SafeOutDir {
    param(
        [Parameter(Mandatory = $true)][string] $RepoRoot,
        [Parameter(Mandatory = $true)][string] $OutDir,
        [switch] $AllowUnsafeOutDir
    )

    function Get-NormalizedFullPath {
        param([Parameter(Mandatory = $true)][string] $Path)

        $full = [System.IO.Path]::GetFullPath($Path)
        $root = [System.IO.Path]::GetPathRoot($full)
        if ($full -eq $root) {
            return $full
        }

        return $full.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    }

    $repoFull = Get-NormalizedFullPath -Path $RepoRoot
    $outFull = Get-NormalizedFullPath -Path $OutDir
    $driveRoot = Get-NormalizedFullPath -Path ([System.IO.Path]::GetPathRoot($outFull))

    if ([string]::IsNullOrWhiteSpace($outFull)) {
        throw "Refusing to use an empty -OutDir."
    }

    if ($outFull -eq $repoFull) {
        throw "Refusing to use -OutDir at the repo root (would delete the working tree): $outFull"
    }
    if ($outFull -eq $driveRoot) {
        throw "Refusing to use -OutDir at the drive root: $outFull"
    }

    $repoOut = Get-NormalizedFullPath -Path (Join-Path $repoFull "out")
    $repoOutPrefix = $repoOut.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    if ($outFull.Equals($repoOut, [System.StringComparison]::OrdinalIgnoreCase) -or $outFull.StartsWith($repoOutPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        return
    }

    if ($AllowUnsafeOutDir) {
        Write-Warning "Using -OutDir outside '$repoOut' because -AllowUnsafeOutDir was provided: $outFull"
        return
    }

    throw "Refusing to use -OutDir outside '$repoOut': $outFull`r`nPass -AllowUnsafeOutDir to override."
}

function Normalize-SigningPolicy {
    param([Parameter(Mandatory = $true)][string] $Policy)

    $p = $Policy.Trim().ToLowerInvariant()
    switch ($p) {
        "testsigning" { return "test" }
        "test-signing" { return "test" }
        "nointegritychecks" { return "none" }
        "no-integrity-checks" { return "none" }
        "prod" { return "production" }
        "whql" { return "production" }
        default { return $p }
    }
}

function Assert-CiPackagedDriversOnly {
    param(
        [Parameter(Mandatory = $true)][string] $InputRoot,
        [Parameter(Mandatory = $true)][string] $RepoRoot
    )

    $driversRoot = Join-Path $RepoRoot "drivers"
    if (-not (Test-Path -LiteralPath $driversRoot -PathType Container)) {
        return
    }

    # Only enforce this gate for the CI packaging layout (out/packages/<driverRel>/<arch>/...).
    # This prevents accidentally shipping stray/stale driver packages that were not explicitly
    # opted into CI packaging via drivers/<driverRel>/ci-package.json.
    # Avoid `-Filter *.inf` because it can be case-sensitive on Linux/macOS; use a
    # case-insensitive extension match so the script works cross-platform.
    $infFiles = @(
        Get-ChildItem -LiteralPath $InputRoot -Recurse -File -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '(?i)\\.inf$' }
    )
    if (-not $infFiles -or $infFiles.Count -eq 0) {
        return
    }

    $inputTrimmed = $InputRoot.TrimEnd("\", "/")
    $archNames = @("x86", "i386", "win32", "x64", "amd64", "x86_64", "x86-64")

    $seen = @{}
    $missing = New-Object System.Collections.Generic.List[object]

    foreach ($inf in $infFiles) {
        $srcDir = Split-Path -Parent $inf.FullName
        $relative = ""
        if ($srcDir.Length -ge $inputTrimmed.Length -and $srcDir.StartsWith($inputTrimmed, [System.StringComparison]::OrdinalIgnoreCase)) {
            $relative = $srcDir.Substring($inputTrimmed.Length).TrimStart("\", "/")
        }
        if ([string]::IsNullOrWhiteSpace($relative)) {
            continue
        }

        $segments = $relative -split "[\\/]+"
        if (-not $segments -or $segments.Count -eq 0) {
            continue
        }

        $archIndex = -1
        for ($i = $segments.Count - 1; $i -ge 0; $i--) {
            $s = $segments[$i].ToLowerInvariant()
            if ($archNames -contains $s) {
                $archIndex = $i
                break
            }
        }

        # If there is no arch directory segment, this INF isn't in the CI packages layout.
        if ($archIndex -le 0) {
            continue
        }

        $driverRel = ($segments[0..($archIndex - 1)] -join "\")
        if ([string]::IsNullOrWhiteSpace($driverRel)) {
            continue
        }

        $key = $driverRel.ToLowerInvariant()
        if ($seen.ContainsKey($key)) {
            continue
        }
        $seen[$key] = $true

        $manifestPath = Join-Path (Join-Path $driversRoot $driverRel) "ci-package.json"
        if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            [void]$missing.Add([pscustomobject]@{
                Driver = $driverRel.Replace("\", "/")
                ExampleInf = $inf.FullName
                Manifest = $manifestPath
            })
        }
    }

    if ($missing.Count -gt 0) {
        $list = ($missing | Sort-Object -Property Driver | ForEach-Object { "- $($_.Driver) (e.g. $($_.ExampleInf))" }) -join "`r`n"
        throw "Refusing to package driver(s) missing drivers/<driver>/ci-package.json.`r`n`r`n$list"
    }
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
        # Prefer SOURCE_DATE_EPOCH when available so artifact naming can be reproducible even
        # in environments without a working git checkout.
        $sde = $env:SOURCE_DATE_EPOCH
        if (-not [string]::IsNullOrWhiteSpace($sde)) {
            try {
                $epoch = [int64] $sde.Trim()
                $date = [DateTimeOffset]::FromUnixTimeSeconds($epoch).ToString("yyyyMMdd", [System.Globalization.CultureInfo]::InvariantCulture)
            } catch {
                $date = $null
            }
        }
        if ($null -eq $date) {
            $date = Get-Date -Format "yyyyMMdd"
        }
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

function Get-DeterministicZipTimestamp {
    param([Parameter(Mandatory = $true)][string] $RepoRoot)

    $zipMin = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
    $zipMax = [DateTimeOffset]::new(2107, 12, 31, 23, 59, 59, [TimeSpan]::Zero)

    $sde = $env:SOURCE_DATE_EPOCH
    if (-not [string]::IsNullOrWhiteSpace($sde)) {
        $epoch = 0
        if ([Int64]::TryParse($sde.Trim(), [ref]$epoch)) {
            $ts = [DateTimeOffset]::FromUnixTimeSeconds($epoch)
            if ($ts -lt $zipMin) { return $zipMin }
            if ($ts -gt $zipMax) { return $zipMax }
            return $ts
        }
    }

    try {
        $ct = (& git -C $RepoRoot show -s --format=%ct HEAD 2>$null).Trim()
        $epoch = 0
        if (-not [string]::IsNullOrWhiteSpace($ct) -and [Int64]::TryParse($ct, [ref]$epoch)) {
            $ts = [DateTimeOffset]::FromUnixTimeSeconds($epoch)
            if ($ts -lt $zipMin) { return $zipMin }
            if ($ts -gt $zipMax) { return $zipMax }
            return $ts
        }
    } catch {
        # ignore and fall back to epoch 0
    }

    return $zipMin
}

function Get-RelativePathForZipEntry {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Path
    )

    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd("\", "/")
    $pathFull = [System.IO.Path]::GetFullPath($Path)
    $prefix = $rootFull + [System.IO.Path]::DirectorySeparatorChar

    $comparison = [System.StringComparison]::Ordinal
    if (Get-IsWindows) {
        $comparison = [System.StringComparison]::OrdinalIgnoreCase
    }

    if (-not $pathFull.StartsWith($prefix, $comparison)) {
        throw "Path '$pathFull' is not under root '$rootFull'."
    }

    return $pathFull.Substring($prefix.Length) -replace "\\", "/"
}

function Test-IsReparsePoint {
    param([Parameter(Mandatory = $true)] $Item)

    try {
        return (($Item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)
    } catch {
        return $false
    }
}

function Test-IsExcludedArtifactRelPath {
    param(
        [Parameter(Mandatory = $true)][string] $RelPath,
        [switch] $IsDirectory
    )

    if ([string]::IsNullOrWhiteSpace($RelPath)) {
        return $true
    }

    $parts = @($RelPath -split "/+")
    foreach ($p in $parts) {
        if ([string]::IsNullOrEmpty($p)) { continue }

        # Hidden files/dirs and macOS metadata directories are not meaningful payload and can cause
        # host-specific nondeterminism in local builds (Finder/Explorer may create these).
        if ($p.StartsWith(".")) { return $true }
        if ($p.Equals("__MACOSX", [System.StringComparison]::OrdinalIgnoreCase)) { return $true }
    }

    if (-not $IsDirectory) {
        $leaf = $parts[$parts.Count - 1]
        $leafLower = $leaf.ToLowerInvariant()
        if ($leafLower -in @("thumbs.db", "ehthumbs.db", "desktop.ini")) {
            return $true
        }
    }

    return $false
}

function Get-DeterministicZipEntriesFromFolder {
    param([Parameter(Mandatory = $true)][string] $Folder)

    $folderFull = [System.IO.Path]::GetFullPath($Folder)
    $entries = New-Object "System.Collections.Generic.List[object]"

    $dirs = @(Get-ChildItem -LiteralPath $folderFull -Recurse -Directory -Force -ErrorAction SilentlyContinue)
    foreach ($d in $dirs) {
        if (Test-IsReparsePoint -Item $d) {
            throw "Refusing to package reparse-point/symlink directory (nondeterministic/unsafe): $($d.FullName)"
        }
        $rel = Get-RelativePathForZipEntry -Root $folderFull -Path $d.FullName
        if (Test-IsExcludedArtifactRelPath -RelPath $rel -IsDirectory) {
            continue
        }
        if ([string]::IsNullOrWhiteSpace($rel)) {
            continue
        }
        $entries.Add([pscustomobject]@{
            EntryName = ($rel.TrimEnd("/") + "/")
            FullPath = $d.FullName
            IsDirectory = $true
        }) | Out-Null
    }

    $files = @(Get-ChildItem -LiteralPath $folderFull -Recurse -File -Force -ErrorAction SilentlyContinue)
    foreach ($f in $files) {
        if (Test-IsReparsePoint -Item $f) {
            throw "Refusing to package reparse-point/symlink file (nondeterministic/unsafe): $($f.FullName)"
        }
        $rel = Get-RelativePathForZipEntry -Root $folderFull -Path $f.FullName
        if (Test-IsExcludedArtifactRelPath -RelPath $rel) {
            continue
        }
        if ([string]::IsNullOrWhiteSpace($rel)) {
            continue
        }
        $entries.Add([pscustomobject]@{
            EntryName = $rel
            FullPath = $f.FullName
            IsDirectory = $false
        }) | Out-Null
    }

    # Stable ordering: sort by full relative path (case-insensitive), then by path (case-sensitive).
    $comparer = [System.Collections.Generic.Comparer[object]]::Create([System.Comparison[object]]{
        param($a, $b)
        $c = [System.StringComparer]::OrdinalIgnoreCase.Compare($a.EntryName, $b.EntryName)
        if ($c -ne 0) {
            return $c
        }
        return [System.StringComparer]::Ordinal.Compare($a.EntryName, $b.EntryName)
    })
    $entries.Sort($comparer)

    # Validate there are no case-insensitive path collisions; these can be produced on Linux/macOS
    # but will collide when extracted on Windows and can cause ambiguous packaging outcomes.
    $seen = @{}
    foreach ($e in $entries) {
        $key = $e.EntryName.ToLowerInvariant()
        if ($seen.ContainsKey($key)) {
            throw "Case-insensitive path collision in '$folderFull': '$($seen[$key])' vs '$($e.EntryName)'. Rename one of the entries to avoid collisions on Windows."
        }
        $seen[$key] = $e.EntryName
    }

    return $entries
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

function Normalize-DriverRel {
    param([Parameter(Mandatory = $true)][string] $Value)

    $normalized = $Value.Replace([System.IO.Path]::DirectorySeparatorChar, "/").Replace([System.IO.Path]::AltDirectorySeparatorChar, "/")
    $normalized = ($normalized -replace "/+", "/").Trim("/")
    return $normalized
}

function Load-DriverNameMap {
    param([string] $Path)

    $map = @{}
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $map
    }

    $resolved = Resolve-RepoPath -Path $Path
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "DriverNameMapJson does not exist: '$resolved'."
    }

    $raw = Get-Content -LiteralPath $resolved -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        throw "DriverNameMapJson is empty: '$resolved'."
    }

    $obj = $raw | ConvertFrom-Json
    if ($null -eq $obj) {
        throw "DriverNameMapJson did not parse as JSON: '$resolved'."
    }

    foreach ($prop in $obj.PSObject.Properties) {
        $kRaw = ([string]$prop.Name).Trim()
        $vRaw = ([string]$prop.Value).Trim()
        if ([string]::IsNullOrWhiteSpace($kRaw) -or [string]::IsNullOrWhiteSpace($vRaw)) {
            continue
        }

        $key = (Normalize-DriverRel -Value $kRaw).ToLowerInvariant()
        $map[$key] = $vRaw
    }

    return $map
}

function Assert-ContainsFileExtension {
    param(
        [Parameter(Mandatory = $true)][string] $Root,
        [Parameter(Mandatory = $true)][string] $Extension
    )

    $ext = $Extension.Trim().TrimStart('.').ToLowerInvariant()
    $pattern = "*.$ext"
    # `-Filter` is case-sensitive on Linux/macOS, so filter manually on the extension.
    $found = Get-ChildItem -Path $Root -Recurse -File -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -and $_.Extension.TrimStart('.').ToLowerInvariant() -eq $ext } |
        Select-Object -First 1
    if (-not $found) {
        throw "Expected at least one '$pattern' file under '$Root'."
    }
}

function Assert-NoUtf8Bom {
    param([Parameter(Mandatory = $true)][string] $Path)

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        throw "Expected '$Path' to be UTF-8 without BOM (found EF BB BF)."
    }
}

function Write-InstallTxt {
    param(
        [Parameter(Mandatory = $true)][string] $DestPath,
        [Parameter(Mandatory = $true)][string] $SigningPolicy
    )

    $normalizedPolicy = Normalize-SigningPolicy -Policy $SigningPolicy

    if ($normalizedPolicy -eq "test") {
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
    } else {
        $lines = @(
            "AeroVirtIO Windows 7 Driver Package",
            "",
            "This folder contains signed driver packages for Windows 7 (x86 + x64).",
            "These drivers are expected to be production/WHQL signed.",
            "No custom test certificate is included in this package.",
            "",
            "1) Install drivers (PnPUtil)",
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
            "2) Windows Setup: \"Load driver\"",
            "--------------------------------",
            "Mount the ISO (if produced) or attach the FAT driver disk image (if produced).",
            "Then choose:",
            "  Install Windows -> Load driver -> Browse",
            "",
            "Driver locations:",
            "  - Bundle ZIP/ISO: drivers\\<driver>\\x86\\*.inf or drivers\\<driver>\\x64\\*.inf",
            "  - FAT disk image: x86\\<driver>\\*.inf or x64\\<driver>\\*.inf",
            "",
            "If you are using test-signed drivers, re-run packaging with -SigningPolicy test to",
            "include aero-test.cer and test signing instructions.",
            ""
        )
    }

    $dir = Split-Path -Parent $DestPath
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    # Make the generated file stable across PowerShell versions/hosts:
    # - UTF-8 without BOM
    # - CRLF newlines (Windows-friendly, deterministic even when running under pwsh on Unix)
    $text = ($lines -join "`r`n") + "`r`n"
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($DestPath, $text, $utf8NoBom)
    Assert-NoUtf8Bom -Path $DestPath
}

function New-DriverArtifactRoot {
    param(
        [Parameter(Mandatory = $true)][string] $DestRoot,
        [Parameter(Mandatory = $true)][string] $SigningPolicy,
        [string] $CertSourcePath,
        [switch] $IncludeCerts
    )

    New-Item -ItemType Directory -Force -Path $DestRoot | Out-Null
    if ($IncludeCerts) {
        if ([string]::IsNullOrWhiteSpace($CertSourcePath)) {
            throw "New-DriverArtifactRoot: CertSourcePath is required when IncludeCerts is set."
        }
        Copy-Item -Path $CertSourcePath -Destination (Join-Path $DestRoot "aero-test.cer") -Force
    }
    Write-InstallTxt -DestPath (Join-Path $DestRoot "INSTALL.txt") -SigningPolicy $SigningPolicy
}

function Copy-DriversForArch {
    param(
        [Parameter(Mandatory = $true)][string] $InputRoot,
        [Parameter(Mandatory = $true)][string[]] $Arches,
        [Parameter(Mandatory = $true)][string] $DestRoot
    )

    $inputRootTrimmed = $InputRoot.TrimEnd("\", "/")

    $infFiles = @(
        Get-ChildItem -Path $InputRoot -Recurse -File -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '(?i)\\.inf$' } |
            Sort-Object -Property FullName
    )
    if (-not $infFiles) {
        throw "No '.inf' files found under '$InputRoot'."
    }

    $seen = New-Object "System.Collections.Generic.HashSet[string]"
    # Detect multiple different source packages mapping to the same destination driver directory
    # (arch + driverName). This prevents accidental merging/overwriting.
    $destDirToSource = New-Object System.Collections.Hashtable ([System.StringComparer]::OrdinalIgnoreCase)
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

        # Driver name overrides are applied before collision detection so callers can
        # disambiguate nested layouts that would otherwise collide (e.g. groupA/blk + groupB/blk).
        $driverRel = ""
        if ($segments -and $segments.Count -gt 0) {
            $archNames = @("x86", "i386", "win32", "x64", "amd64", "x86_64", "x86-64")
            $archIndex = -1
            for ($i = $segments.Count - 1; $i -ge 0; $i--) {
                if ($archNames -contains $segments[$i].ToLowerInvariant()) {
                    $archIndex = $i
                    break
                }
            }
            if ($archIndex -gt 0) {
                $driverRel = ($segments[0..($archIndex - 1)] -join "/")
            } else {
                $driverRel = ($segments -join "/")
            }
        }
        $driverRelNorm = ""
        if (-not [string]::IsNullOrWhiteSpace($driverRel)) {
            $driverRelNorm = Normalize-DriverRel -Value $driverRel
        }
        $driverNameNormKey = (Normalize-PathComponent -Value $driverName.Trim()).ToLowerInvariant()
        $driverRelKey = $driverRelNorm.ToLowerInvariant()
        if (-not [string]::IsNullOrWhiteSpace($driverRelKey) -and $script:DriverNameMap.ContainsKey($driverRelKey)) {
            $driverName = $script:DriverNameMap[$driverRelKey]
        } elseif ($script:DriverNameMap.ContainsKey($driverNameNormKey)) {
            $driverName = $script:DriverNameMap[$driverNameNormKey]
        }

        $driverName = Normalize-PathComponent -Value $driverName

        $destDir = Join-Path $DestRoot (Join-Path "drivers" (Join-Path $driverName $arch))

        $destKey = ("{0}|{1}" -f $arch, $driverName)
        if ($destDirToSource.ContainsKey($destKey)) {
            $existing = $destDirToSource[$destKey]
            $existingSrc = [string]$existing.SourceDir
            if (-not $existingSrc.Equals($srcDir, [System.StringComparison]::OrdinalIgnoreCase)) {
                $msg = @(
                    "Driver name collision detected while staging signed drivers.",
                    "",
                    "Destination directory (would be merged/overwritten):",
                    "  $destDir",
                    "",
                    "The following two source directories both map to the same destination (arch=$arch, driverName=$driverName):",
                    "",
                    "  1) $existingSrc",
                    "     driverRel: $($existing.DriverRel)",
                    "     INF      : $($existing.Inf)",
                    "",
                    "  2) $srcDir",
                    "     driverRel: $driverRelNorm",
                    "     INF      : $($inf.FullName)",
                    "",
                    "Remediation:",
                    "  - Rename one of the driver package directories so it maps to a unique driver folder name, OR",
                    "  - Provide an explicit mapping via -DriverNameMapJson (see ci/README.md)."
                ) -join "`r`n"
                throw $msg
            }
        } else {
            $destDirToSource[$destKey] = [pscustomobject]@{
                SourceDir = $srcDir
                Inf = $inf.FullName
                DriverRel = $driverRelNorm
            }
        }

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

function Invoke-PackageDriversSelfTest {
    # Unit-like self-test for collision detection (and optional mapping overrides).
    # Run:
    #   pwsh -File ci/package-drivers.ps1 -SelfTest
    $root = Resolve-RepoPath -Path "out/_selftest_package_drivers"
    if (Test-Path -LiteralPath $root) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $root | Out-Null

    $inputRoot = Join-Path $root "input"
    $stageRoot = Join-Path $root "stage"
    New-Item -ItemType Directory -Force -Path $inputRoot | Out-Null
    New-Item -ItemType Directory -Force -Path $stageRoot | Out-Null

    # Two different packages with the same leaf driver directory ("blk") under different groups.
    # Without an explicit mapping, Get-DriverNameFromRelativeSegments maps both to "blk",
    # causing a collision at: drivers/blk/x64.
    $pkg1 = Join-Path $inputRoot "groupA/blk/x64"
    $pkg2 = Join-Path $inputRoot "groupB/blk/x64"
    New-Item -ItemType Directory -Force -Path $pkg1 | Out-Null
    New-Item -ItemType Directory -Force -Path $pkg2 | Out-Null

    Set-Content -LiteralPath (Join-Path $pkg1 "a.inf") -Value "; test" -Encoding Ascii
    New-Item -ItemType File -Force -Path (Join-Path $pkg1 "a.cat") | Out-Null
    New-Item -ItemType File -Force -Path (Join-Path $pkg1 "a.sys") | Out-Null

    Set-Content -LiteralPath (Join-Path $pkg2 "b.inf") -Value "; test" -Encoding Ascii
    New-Item -ItemType File -Force -Path (Join-Path $pkg2 "b.cat") | Out-Null
    New-Item -ItemType File -Force -Path (Join-Path $pkg2 "b.sys") | Out-Null

    $script:DriverNameMap = @{}
    $threw = $false
    try {
        Copy-DriversForArch -InputRoot $inputRoot -Arches @("x64") -DestRoot $stageRoot
    } catch {
        $threw = $true
        $m = $_.Exception.Message
        if ($m -notmatch "Driver name collision detected") {
            throw "SelfTest: expected collision error, got: $m"
        }
    }
    if (-not $threw) {
        throw "SelfTest: expected Copy-DriversForArch to throw on driver-name collision, but it succeeded."
    }

    # Demonstrate explicit mapping to disambiguate the output folder names.
    $mapPath = Join-Path $root "driver-name-map.json"
    $mapJson = @(
        "{",
        "  `"groupA/blk`": `"virtio-blk-a`",",
        "  `"groupB/blk`": `"virtio-blk-b`"",
        "}"
    ) -join "`r`n"
    $mapJson | Set-Content -LiteralPath $mapPath -Encoding UTF8

    $script:DriverNameMap = Load-DriverNameMap -Path $mapPath
    if (Test-Path -LiteralPath $stageRoot) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $stageRoot | Out-Null

    Copy-DriversForArch -InputRoot $inputRoot -Arches @("x64") -DestRoot $stageRoot

    $expected1 = Join-Path $stageRoot "drivers/virtio-blk-a/x64"
    $expected2 = Join-Path $stageRoot "drivers/virtio-blk-b/x64"
    if (-not (Test-Path -LiteralPath $expected1 -PathType Container)) {
        throw "SelfTest: expected mapped output directory not found: $expected1"
    }
    if (-not (Test-Path -LiteralPath $expected2 -PathType Container)) {
        throw "SelfTest: expected mapped output directory not found: $expected2"
    }

    # Signing-policy smoke checks (no ISO / no manifests): ensure certificate and test-signing
    # instructions are only included for SigningPolicy=test.
    try {
        $testInput = Resolve-RepoPath -Path "tools/packaging/aero_packager/testdata/drivers"
        if (-not (Test-Path -LiteralPath $testInput -PathType Container)) {
            throw "SelfTest: expected test drivers directory not found: $testInput"
        }

        $certCandidate = Resolve-RepoPath -Path "guest-tools/certs/AeroTestRoot.cer"
        if (-not (Test-Path -LiteralPath $certCandidate -PathType Leaf)) {
            throw "SelfTest: expected test certificate not found: $certCandidate"
        }

        $outTest = Join-Path $root "out-policy-test"
        $outNone = Join-Path $root "out-policy-none"
        if (Test-Path -LiteralPath $outTest) { Remove-Item -LiteralPath $outTest -Recurse -Force }
        if (Test-Path -LiteralPath $outNone) { Remove-Item -LiteralPath $outNone -Recurse -Force }

        $script:DriverNameMap = @{}
        $rTest = Invoke-PackageDrivers -InputRoot $testInput -SigningPolicy "test" -CertPath $certCandidate -OutDir $outTest -RepoRoot "." -Version "0.0.0" -NoIso -MakeFatImage:$false -NoManifest
        $rNone = Invoke-PackageDrivers -InputRoot $testInput -SigningPolicy "none" -CertPath $certCandidate -OutDir $outNone -RepoRoot "." -Version "0.0.0" -NoIso -MakeFatImage:$false -NoManifest

        Add-Type -AssemblyName System.IO.Compression -ErrorAction Stop | Out-Null

        function Read-ZipEntryText {
            param(
                [Parameter(Mandatory = $true)][string] $ZipPath,
                [Parameter(Mandatory = $true)][string] $EntryPath
            )

            $fs = [System.IO.File]::OpenRead($ZipPath)
            $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
            try {
                $entry = $zip.GetEntry($EntryPath)
                if (-not $entry) {
                    throw "SelfTest: expected ZIP '$ZipPath' to contain entry '$EntryPath'."
                }
                $s = $entry.Open()
                try {
                    $sr = New-Object System.IO.StreamReader($s, [System.Text.Encoding]::UTF8, $true)
                    try {
                        return $sr.ReadToEnd()
                    } finally {
                        $sr.Dispose()
                    }
                } finally {
                    $s.Dispose()
                }
            } finally {
                $zip.Dispose()
                $fs.Dispose()
            }
        }

        function Test-ZipHasEntry {
            param(
                [Parameter(Mandatory = $true)][string] $ZipPath,
                [Parameter(Mandatory = $true)][string] $EntryPath
            )

            $fs = [System.IO.File]::OpenRead($ZipPath)
            $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Read, $false)
            try {
                return $null -ne $zip.GetEntry($EntryPath)
            } finally {
                $zip.Dispose()
                $fs.Dispose()
            }
        }

        if (-not (Test-ZipHasEntry -ZipPath $rTest.ZipBundle -EntryPath "aero-test.cer")) {
            throw "SelfTest: expected SigningPolicy=test bundle ZIP to include aero-test.cer."
        }
        if (Test-ZipHasEntry -ZipPath $rNone.ZipBundle -EntryPath "aero-test.cer") {
            throw "SelfTest: expected SigningPolicy=none bundle ZIP to NOT include aero-test.cer."
        }

        $installTest = Read-ZipEntryText -ZipPath $rTest.ZipBundle -EntryPath "INSTALL.txt"
        if ($installTest -notmatch "bcdedit\\s+/set\\s+testsigning\\s+on") {
            throw "SelfTest: expected SigningPolicy=test INSTALL.txt to include testsigning instructions."
        }
        if ($installTest -notmatch "certutil\\s+-addstore\\s+-f\\s+Root\\s+aero-test\\.cer") {
            throw "SelfTest: expected SigningPolicy=test INSTALL.txt to include certificate import instructions."
        }

        $installNone = Read-ZipEntryText -ZipPath $rNone.ZipBundle -EntryPath "INSTALL.txt"
        if ($installNone -match "bcdedit\\s+/set\\s+testsigning\\s+on") {
            throw "SelfTest: expected SigningPolicy=none INSTALL.txt to NOT include testsigning instructions."
        }
        if ($installNone -match "certutil\\s+-addstore\\s+-f\\s+Root\\s+aero-test\\.cer") {
            throw "SelfTest: expected SigningPolicy=none INSTALL.txt to NOT include certificate import instructions."
        }
        if ($installNone -match "The certificate is included as:\\s+aero-test\\.cer") {
            throw "SelfTest: expected SigningPolicy=none INSTALL.txt to NOT claim a certificate is included."
        }
    } finally {
        if (Test-Path -LiteralPath (Join-Path $root "out-policy-test")) { Remove-Item -LiteralPath (Join-Path $root "out-policy-test") -Recurse -Force -ErrorAction SilentlyContinue }
        if (Test-Path -LiteralPath (Join-Path $root "out-policy-none")) { Remove-Item -LiteralPath (Join-Path $root "out-policy-none") -Recurse -Force -ErrorAction SilentlyContinue }
    }

    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

function New-FatImageInputFromBundle {
    param(
        [Parameter(Mandatory = $true)][string] $BundleRoot,
        [Parameter(Mandatory = $true)][string] $SigningPolicy,
        [string] $CertSourcePath,
        [Parameter(Mandatory = $true)][string] $DestRoot,
        [switch] $IncludeCerts
    )

    New-DriverArtifactRoot -DestRoot $DestRoot -SigningPolicy $SigningPolicy -CertSourcePath $CertSourcePath -IncludeCerts:$IncludeCerts

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
        [Parameter(Mandatory = $true)][string] $ZipPath,
        [Parameter(Mandatory = $true)][DateTimeOffset] $DeterministicTimestamp
    )

    if (Test-Path $ZipPath) {
        Remove-Item -Force $ZipPath
    }

    try {
        # `Compress-Archive` is not deterministic (timestamps/file order vary across hosts).
        # Use ZipArchive directly and fail if unavailable so callers do not silently get
        # non-reproducible artifacts.
        Add-Type -AssemblyName System.IO.Compression -ErrorAction Stop | Out-Null

        $entries = Get-DeterministicZipEntriesFromFolder -Folder $Folder

        $fs = [System.IO.File]::Open($ZipPath, [System.IO.FileMode]::CreateNew)
        try {
            $zip = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create, $false)
            try {
                foreach ($e in $entries) {
                    if ($e.IsDirectory) {
                        $entry = $zip.CreateEntry($e.EntryName)
                        $entry.LastWriteTime = $DeterministicTimestamp
                        try {
                            # Best-effort: avoid capturing unpredictable host attributes.
                            # Mark directory attribute but keep everything else stable.
                            $entry.ExternalAttributes = 0x10
                        } catch {
                        }
                        continue
                    }

                    $entry = $zip.CreateEntry($e.EntryName, [System.IO.Compression.CompressionLevel]::Optimal)
                    $entry.LastWriteTime = $DeterministicTimestamp
                    try {
                        # Best-effort: avoid capturing unpredictable host attributes.
                        $entry.ExternalAttributes = 0
                    } catch {
                    }

                    $inStream = [System.IO.File]::OpenRead($e.FullPath)
                    try {
                        $outStream = $entry.Open()
                        try {
                            $inStream.CopyTo($outStream)
                        } finally {
                            $outStream.Dispose()
                        }
                    } finally {
                        $inStream.Dispose()
                    }
                }
            } finally {
                $zip.Dispose()
            }
        } finally {
            $fs.Dispose()
        }
    } catch {
        # Avoid leaving partial output files around.
        if (Test-Path -LiteralPath $ZipPath) {
            Remove-Item -LiteralPath $ZipPath -Force -ErrorAction SilentlyContinue
        }
        throw
    }

    $zipFile = Get-Item -Path $ZipPath -ErrorAction SilentlyContinue
    if (-not $zipFile -or $zipFile.Length -le 0) {
        throw "Failed to create ZIP, or ZIP is empty: '$ZipPath'."
    }
}

function New-IsoFromFolder {
    param(
        [Parameter(Mandatory = $true)][string] $Folder,
        [Parameter(Mandatory = $true)][string] $IsoPath,
        [Parameter(Mandatory = $true)][string] $VolumeLabel,
        [Nullable[long]] $SourceDateEpoch
    )

    if (Test-Path $IsoPath) {
        Remove-Item -Force $IsoPath
    }

    # Deterministic ISO creation uses the Rust ISO writer (`aero_iso`).
    # This requires `cargo` and works cross-platform.
    #
    # Use `-LegacyIso` to force the legacy Windows IMAPI2 implementation (not deterministic).
    $cargoExe = (Get-Command cargo -ErrorAction SilentlyContinue).Source
    if (-not $LegacyIso) {
        if ([string]::IsNullOrWhiteSpace($cargoExe)) {
            $msg = "ISO creation requires Rust/cargo for deterministic, cross-platform builds. Install Rust/cargo, or re-run with -NoIso."
            if (Get-IsWindows) {
                $msg += " (On Windows, you may pass -LegacyIso to use IMAPI2, but the ISO will not be deterministic.)"
            }
            throw $msg
        }

        $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
        $manifestPath = Join-Path $repoRoot "tools/packaging/aero_packager/Cargo.toml"
        if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            throw "Missing aero_packager Cargo.toml: '$manifestPath'."
        }

        $sourceDateEpoch = 0
        if ($null -ne $SourceDateEpoch) {
            $sourceDateEpoch = [int64] $SourceDateEpoch
        } elseif (-not [string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
            try {
                $sourceDateEpoch = [int64] $env:SOURCE_DATE_EPOCH
            } catch {
                throw "Invalid SOURCE_DATE_EPOCH (expected integer seconds): '$($env:SOURCE_DATE_EPOCH)'."
            }
        }

        & $cargoExe run --quiet --release --locked --manifest-path $manifestPath --bin aero_iso -- `
            --in-dir $Folder `
            --out-iso $IsoPath `
            --volume-id $VolumeLabel `
            --source-date-epoch $sourceDateEpoch
        if ($LASTEXITCODE -ne 0) {
            throw "Deterministic ISO creation failed (cargo exit code $LASTEXITCODE)."
        }
    } else {
        if (-not (Get-IsWindows)) {
            throw "ISO creation with -LegacyIso requires Windows (IMAPI2)."
        }

        $helper = Join-Path $PSScriptRoot "lib/New-IsoFile.ps1"
        if (-not (Test-Path $helper)) {
            throw "Missing helper script: '$helper'."
        }

        $powershellExe = (Get-Command powershell.exe -ErrorAction SilentlyContinue).Source
        if ($powershellExe) {
            & $powershellExe -NoProfile -ExecutionPolicy Bypass -STA -File $helper -SourcePath $Folder -IsoPath $IsoPath -VolumeLabel $VolumeLabel -SourceDateEpoch $SourceDateEpoch -LegacyIso:$LegacyIso
            if ($LASTEXITCODE -ne 0) {
                throw "ISO creation failed (exit code $LASTEXITCODE)."
            }
        } else {
            . $helper
            New-IsoFile -SourcePath $Folder -IsoPath $IsoPath -VolumeLabel $VolumeLabel -SourceDateEpoch $SourceDateEpoch -LegacyIso:$LegacyIso
        }
    }

    $isoFile = Get-Item -Path $IsoPath -ErrorAction SilentlyContinue
    if (-not $isoFile -or $isoFile.Length -le 0) {
        throw "ISO file was not created or is empty: '$IsoPath'."
    }
}

function Get-BuildIdString {
    $candidates = @(
        $env:GITHUB_RUN_ID,
        $env:GITHUB_RUN_NUMBER,
        $env:BUILD_BUILDID,
        $env:CI_PIPELINE_ID
    )
    foreach ($c in $candidates) {
        if (-not [string]::IsNullOrWhiteSpace($c)) {
            return ([string]$c).Trim()
        }
    }
    return $null
}

function Get-DefaultSourceDateEpoch {
    param([Parameter(Mandatory = $true)][string] $RepoRoot)

    if (-not [string]::IsNullOrWhiteSpace($env:SOURCE_DATE_EPOCH)) {
        $epoch = 0
        if ([Int64]::TryParse($env:SOURCE_DATE_EPOCH.Trim(), [ref]$epoch)) {
            return [long] $epoch
        }
    }

    try {
        $ct = (& git -C $RepoRoot show -s --format=%ct HEAD 2>$null).Trim()
        $epoch = 0
        if (-not [string]::IsNullOrWhiteSpace($ct) -and [Int64]::TryParse($ct, [ref]$epoch)) {
            return [long] $epoch
        }
    } catch {
        # fall through
    }

    return 0
}

function Get-ManifestFileEntries {
    param([Parameter(Mandatory = $true)][string] $Root)

    $entries = Get-DeterministicZipEntriesFromFolder -Folder $Root
    $files = New-Object "System.Collections.Generic.List[object]"

    foreach ($e in $entries) {
        if ($e.IsDirectory) { continue }

        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $e.FullPath).Hash.ToLowerInvariant()
        $size = [long] (Get-Item -LiteralPath $e.FullPath).Length

        $files.Add([pscustomobject]([ordered]@{
            path   = $e.EntryName
            sha256 = $hash
            size   = $size
        })) | Out-Null
    }

    # Stable ordering: sort by path (ordinal, case-sensitive).
    $comparer = [System.Collections.Generic.Comparer[object]]::Create([System.Comparison[object]]{
        param($a, $b)
        return [System.StringComparer]::Ordinal.Compare($a.path, $b.path)
    })
    $files.Sort($comparer)

    return $files.ToArray()
}

function Write-JsonUtf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string] $Path,
        [Parameter(Mandatory = $true)][string] $JsonText
    )

    $dir = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }

    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, ($JsonText + "`n"), $utf8NoBom)
    Assert-NoUtf8Bom -Path $Path
}

function Write-IntegrityManifest {
    param(
        [Parameter(Mandatory = $true)][string] $ManifestPath,
        [Parameter(Mandatory = $true)][string] $ArtifactPath,
        [Parameter(Mandatory = $true)][string] $SigningPolicy,
        [Parameter(Mandatory = $true)][string] $PackageName,
        [Parameter(Mandatory = $true)][string] $Version,
        [Parameter(Mandatory = $true)][long] $SourceDateEpoch,
        [string] $BuildId,
        [Parameter(Mandatory = $true)] $Files
    )

    $artifact = Get-Item -LiteralPath $ArtifactPath -ErrorAction SilentlyContinue
    if (-not $artifact -or $artifact.Length -le 0) {
        throw "Expected artifact to exist and be non-empty: '$ArtifactPath'."
    }
    $artifactSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArtifactPath).Hash.ToLowerInvariant()
    $artifactLeaf = Split-Path -Leaf $ArtifactPath

    $normalizedPolicy = Normalize-SigningPolicy -Policy $SigningPolicy

    $pkg = [ordered]@{
        name    = $PackageName
        version = $Version
    }
    if (-not [string]::IsNullOrWhiteSpace($BuildId)) {
        $pkg.build_id = $BuildId
    }
    $pkg.source_date_epoch = [long] $SourceDateEpoch

    $manifest = [ordered]@{
        schema_version = 1
        path          = $artifactLeaf
        size          = [long] $artifact.Length
        sha256        = $artifactSha256
        version       = $Version
        signing_policy = $normalizedPolicy
        package        = $pkg
        # Force JSON array output even if the file list has only one entry.
        files          = @($Files)
    }

    $json = $manifest | ConvertTo-Json -Depth 10 -Compress
    Write-JsonUtf8NoBom -Path $ManifestPath -JsonText $json

    # Basic sanity check: ensure the written manifest parses as JSON.
    try {
        $null = (Get-Content -LiteralPath $ManifestPath -Raw -ErrorAction Stop) | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw "Generated manifest did not parse as JSON: '$ManifestPath'. $($_.Exception.Message)"
    }
}

function Invoke-PackageDrivers {
    param(
        [Parameter(Mandatory = $true)][string] $InputRoot,
        [Parameter(Mandatory = $true)][string] $SigningPolicy,
        [Parameter(Mandatory = $true)][string] $CertPath,
        [Parameter(Mandatory = $true)][string] $OutDir,
        [Parameter(Mandatory = $true)][string] $RepoRoot,
        [string] $Version,
        [switch] $NoIso,
        [switch] $MakeFatImage,
        [switch] $FatImageStrict,
        [int] $FatImageSizeMB = 64,
        [switch] $NoManifest,
        [switch] $AllowUnsafeOutDir
    )

    $inputRootResolved = Resolve-RepoPath -Path $InputRoot
    $certPathResolved = Resolve-RepoPath -Path $CertPath
    $outDirResolved = Resolve-RepoPath -Path $OutDir
    $repoRootResolved = Resolve-RepoPath -Path $RepoRoot

    $SigningPolicy = Normalize-SigningPolicy -Policy $SigningPolicy
    $includeCerts = $SigningPolicy -eq "test"

    Assert-SafeOutDir -RepoRoot $repoRootResolved -OutDir $outDirResolved -AllowUnsafeOutDir:$AllowUnsafeOutDir

    if (-not (Test-Path $inputRootResolved)) {
        throw "InputRoot does not exist: '$inputRootResolved'."
    }
    if ($includeCerts) {
        if (-not (Test-Path -LiteralPath $certPathResolved -PathType Leaf)) {
            throw "CertPath does not exist: '$certPathResolved'."
        }
    }

    Assert-CiPackagedDriversOnly -InputRoot $inputRootResolved -RepoRoot $repoRootResolved

    if ([string]::IsNullOrWhiteSpace($Version)) {
        $Version = Get-VersionString
    }

    $sourceDateEpoch = Get-DefaultSourceDateEpoch -RepoRoot $repoRootResolved
    $zipTimestamp = Get-DeterministicZipTimestamp -RepoRoot $repoRootResolved

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
    # Safety guard: `Remove-Item -Recurse -Force $stagingBase` must never be able to delete
    # arbitrary directories if OutDir is misconfigured.
    $outFull = [System.IO.Path]::GetFullPath($outDirResolved).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $stagingFull = [System.IO.Path]::GetFullPath($stagingBase).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $comparison = if (Get-IsWindows) { [System.StringComparison]::OrdinalIgnoreCase } else { [System.StringComparison]::Ordinal }
    $sep = [System.IO.Path]::DirectorySeparatorChar
    if (-not $stagingFull.StartsWith($outFull + $sep, $comparison)) {
        throw "Internal error: staging directory is not a subdirectory of OutDir. OutDir='$outFull' staging='$stagingFull'"
    }
    if (Test-Path $stagingBase) {
        Remove-Item -Recurse -Force $stagingBase
    }
    New-Item -ItemType Directory -Force -Path $stagingBase | Out-Null

    $stageX86 = Join-Path $stagingBase "x86"
    $stageX64 = Join-Path $stagingBase "x64"
    $stageBundle = Join-Path $stagingBase "bundle"
    $stageFat = Join-Path $stagingBase "fat"

    New-DriverArtifactRoot -DestRoot $stageX86 -SigningPolicy $SigningPolicy -CertSourcePath $certPathResolved -IncludeCerts:$includeCerts
    New-DriverArtifactRoot -DestRoot $stageX64 -SigningPolicy $SigningPolicy -CertSourcePath $certPathResolved -IncludeCerts:$includeCerts
    New-DriverArtifactRoot -DestRoot $stageBundle -SigningPolicy $SigningPolicy -CertSourcePath $certPathResolved -IncludeCerts:$includeCerts

    Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x86") -DestRoot $stageX86
    Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x64") -DestRoot $stageX64
    Copy-DriversForArch -InputRoot $inputRootResolved -Arches @("x86", "x64") -DestRoot $stageBundle

    $shouldWriteManifests = -not $NoManifest
    $buildId = $null
    $epoch = $sourceDateEpoch
    $manifestFilesX86 = $null
    $manifestFilesX64 = $null
    $manifestFilesBundle = $null
    $manifestFilesFat = $null

    $manifestX86 = $null
    $manifestX64 = $null
    $manifestBundle = $null
    $manifestIso = $null
    $manifestFat = $null

    if ($shouldWriteManifests) {
        $manifestX86 = [System.IO.Path]::ChangeExtension($zipX86, "manifest.json")
        $manifestX64 = [System.IO.Path]::ChangeExtension($zipX64, "manifest.json")
        $manifestBundle = [System.IO.Path]::ChangeExtension($zipBundle, "manifest.json")
        if (-not $NoIso) {
            $manifestIso = [System.IO.Path]::ChangeExtension($isoBundle, "manifest.json")
        }
        if ($shouldMakeFatImage) {
            $manifestFat = [System.IO.Path]::ChangeExtension($fatVhd, "manifest.json")
        }

        $buildId = Get-BuildIdString

        # Enumerate and hash staged files BEFORE archiving; these entries must match the staged bytes.
        $manifestFilesX86 = Get-ManifestFileEntries -Root $stageX86
        $manifestFilesX64 = Get-ManifestFileEntries -Root $stageX64
        $manifestFilesBundle = Get-ManifestFileEntries -Root $stageBundle
    }

    $success = $false
    try {
        New-ZipFromFolder -Folder $stageX86 -ZipPath $zipX86 -DeterministicTimestamp $zipTimestamp
        New-ZipFromFolder -Folder $stageX64 -ZipPath $zipX64 -DeterministicTimestamp $zipTimestamp
        New-ZipFromFolder -Folder $stageBundle -ZipPath $zipBundle -DeterministicTimestamp $zipTimestamp

        if (-not $NoIso) {
            $label = ("AEROVIRTIO_WIN7_" + $Version).ToUpperInvariant() -replace "[^A-Z0-9_]", "_"
            if ($label.Length -gt 32) {
                $label = $label.Substring(0, 32)
            }
            New-IsoFromFolder -Folder $stageBundle -IsoPath $isoBundle -VolumeLabel $label -SourceDateEpoch $epoch
        }

        if ($shouldMakeFatImage) {
            New-FatImageInputFromBundle -BundleRoot $stageBundle -SigningPolicy $SigningPolicy -CertSourcePath $certPathResolved -DestRoot $stageFat -IncludeCerts:$includeCerts
            if ($shouldWriteManifests) {
                $manifestFilesFat = Get-ManifestFileEntries -Root $stageFat
            }

            $helper = Join-Path $PSScriptRoot "make-fat-image.ps1"
            if (-not (Test-Path $helper)) {
                throw "Missing helper script: '$helper'."
            }

            & $helper -SourceDir $stageFat -OutFile $fatVhd -SizeMB $FatImageSizeMB -Strict:$FatImageStrict -SigningPolicy $SigningPolicy
        }

        if ($shouldWriteManifests) {
            # Keep package.name stable across releases; version is encoded separately.
            Write-IntegrityManifest -ManifestPath $manifestX86 -ArtifactPath $zipX86 -SigningPolicy $SigningPolicy -PackageName "AeroVirtIO-Win7-x86" -Version $Version -SourceDateEpoch $epoch -BuildId $buildId -Files $manifestFilesX86
            Write-IntegrityManifest -ManifestPath $manifestX64 -ArtifactPath $zipX64 -SigningPolicy $SigningPolicy -PackageName "AeroVirtIO-Win7-x64" -Version $Version -SourceDateEpoch $epoch -BuildId $buildId -Files $manifestFilesX64
            Write-IntegrityManifest -ManifestPath $manifestBundle -ArtifactPath $zipBundle -SigningPolicy $SigningPolicy -PackageName "AeroVirtIO-Win7-bundle" -Version $Version -SourceDateEpoch $epoch -BuildId $buildId -Files $manifestFilesBundle

            if (-not $NoIso) {
                Write-IntegrityManifest -ManifestPath $manifestIso -ArtifactPath $isoBundle -SigningPolicy $SigningPolicy -PackageName "AeroVirtIO-Win7" -Version $Version -SourceDateEpoch $epoch -BuildId $buildId -Files $manifestFilesBundle
            }

            if ($shouldMakeFatImage -and (Test-Path -LiteralPath $fatVhd -PathType Leaf)) {
                if ($null -eq $manifestFilesFat) {
                    # Defensive: stageFat is populated in the FAT image path, but ensure we have the
                    # staged file list if that step was skipped for any reason.
                    $manifestFilesFat = Get-ManifestFileEntries -Root $stageFat
                }
                if ($null -eq $manifestFat) {
                    $manifestFat = [System.IO.Path]::ChangeExtension($fatVhd, "manifest.json")
                }
                Write-IntegrityManifest -ManifestPath $manifestFat -ArtifactPath $fatVhd -SigningPolicy $SigningPolicy -PackageName "AeroVirtIO-Win7-fat" -Version $Version -SourceDateEpoch $epoch -BuildId $buildId -Files $manifestFilesFat
            }
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
    if ($shouldWriteManifests) {
        Write-Host "  $manifestX86"
        Write-Host "  $manifestX64"
        Write-Host "  $manifestBundle"
        if (-not $NoIso) {
            Write-Host "  $manifestIso"
        }
        if ($shouldMakeFatImage -and (Test-Path -LiteralPath $fatVhd -PathType Leaf)) {
            Write-Host "  $manifestFat"
        }
    }

    if ($shouldMakeFatImage) {
        if (Test-Path $fatVhd) {
            Write-Host "  $fatVhd"
        } else {
            Write-Host "  (FAT image skipped)"
        }
    }

    return [pscustomobject]@{
        Version = $Version
        ZipX86 = $zipX86
        ZipX64 = $zipX64
        ZipBundle = $zipBundle
        IsoBundle = $isoBundle
        ManifestX86 = $manifestX86
        ManifestX64 = $manifestX64
        ManifestBundle = $manifestBundle
        ManifestIso = $manifestIso
        ManifestFat = $manifestFat
        FatVhd = $fatVhd
        OutDir = $outDirResolved
    }
}

function Invoke-DeterminismSelfTest {
    param(
        [Parameter(Mandatory = $true)][string] $InputRoot,
        [Parameter(Mandatory = $true)][string] $SigningPolicy,
        [Parameter(Mandatory = $true)][string] $CertPath,
        [Parameter(Mandatory = $true)][string] $RepoRoot,
        [string] $Version
    )

    $repoRootResolved = Resolve-RepoPath -Path $RepoRoot

    # Make self-test runnable without pre-built drivers/certs by falling back to checked-in fixtures.
    $inputCandidate = Resolve-RepoPath -Path $InputRoot
    if (-not (Test-Path $inputCandidate)) {
        $inputCandidate = Resolve-RepoPath -Path "tools/packaging/aero_packager/testdata/drivers"
    }

    $certCandidate = Resolve-RepoPath -Path $CertPath
    if (-not (Test-Path $certCandidate)) {
        $certCandidate = Resolve-RepoPath -Path "guest-tools/certs/AeroTestRoot.cer"
    }

    if ([string]::IsNullOrWhiteSpace($Version)) {
        $Version = Get-VersionString
    }

    $tempBase = Join-Path ([System.IO.Path]::GetTempPath()) ("aero-package-drivers-determinism-" + [System.Guid]::NewGuid().ToString("N"))
    $out1 = Join-Path $tempBase "run1"
    $out2 = Join-Path $tempBase "run2"

    # Only test ISO determinism when we can create an ISO (requires cargo).
    # If cargo is unavailable, the self-test disables ISO creation via -NoIso.
    $cargoExe = (Get-Command cargo -ErrorAction SilentlyContinue).Source
    $canTestIso = -not [string]::IsNullOrWhiteSpace($cargoExe)
    $noIso = -not $canTestIso

    $oldFatEnv = $env:AERO_MAKE_FAT_IMAGE
    try {
        # Ensure the env-var toggle doesn't accidentally make the self-test depend on
        # external FAT image tooling.
        $env:AERO_MAKE_FAT_IMAGE = "0"

        $r1 = Invoke-PackageDrivers -InputRoot $inputCandidate -SigningPolicy $SigningPolicy -CertPath $certCandidate -OutDir $out1 -RepoRoot $repoRootResolved -Version $Version -NoIso:$noIso -MakeFatImage:$false -NoManifest -AllowUnsafeOutDir
        $r2 = Invoke-PackageDrivers -InputRoot $inputCandidate -SigningPolicy $SigningPolicy -CertPath $certCandidate -OutDir $out2 -RepoRoot $repoRootResolved -Version $Version -NoIso:$noIso -MakeFatImage:$false -NoManifest -AllowUnsafeOutDir

        $checks = @(
            @{ Name = "x86 ZIP"; Path1 = $r1.ZipX86; Path2 = $r2.ZipX86 },
            @{ Name = "x64 ZIP"; Path1 = $r1.ZipX64; Path2 = $r2.ZipX64 },
            @{ Name = "bundle ZIP"; Path1 = $r1.ZipBundle; Path2 = $r2.ZipBundle }
        )
        if ($canTestIso) {
            $checks += @{ Name = "bundle ISO"; Path1 = $r1.IsoBundle; Path2 = $r2.IsoBundle }
        }

        $results = New-Object System.Collections.Generic.List[object]
        foreach ($c in $checks) {
            $p1 = [string]$c.Path1
            $p2 = [string]$c.Path2
            if (-not (Test-Path -LiteralPath $p1 -PathType Leaf)) {
                throw "Determinism self-test internal error: expected output missing: $p1"
            }
            if (-not (Test-Path -LiteralPath $p2 -PathType Leaf)) {
                throw "Determinism self-test internal error: expected output missing: $p2"
            }
            $h1 = (Get-FileHash -Algorithm SHA256 -LiteralPath $p1).Hash
            $h2 = (Get-FileHash -Algorithm SHA256 -LiteralPath $p2).Hash
            if ($h1 -ne $h2) {
                throw "Determinism self-test failed: $($c.Name) SHA-256 mismatch.`r`n  Run1: $p1 -> $h1`r`n  Run2: $p2 -> $h2`r`nTemp dir preserved for inspection: $tempBase"
            }
            $results.Add([pscustomobject]@{ Name = $c.Name; Sha256 = $h1 }) | Out-Null
        }

        $summary = ($results | ForEach-Object { "$($_.Name)=$($_.Sha256)" }) -join "; "
        Write-Host "Determinism self-test passed ($summary)."
        if (-not $canTestIso) {
            Write-Host "  (ISO check skipped because cargo is not available.)"
        }
        Remove-Item -Recurse -Force $tempBase -ErrorAction SilentlyContinue
    } finally {
        if ($null -eq $oldFatEnv) {
            Remove-Item env:AERO_MAKE_FAT_IMAGE -ErrorAction SilentlyContinue
        } else {
            $env:AERO_MAKE_FAT_IMAGE = $oldFatEnv
        }
    }
}

if ($SelfTest) {
    Invoke-PackageDriversSelfTest
    Write-Host "ci/package-drivers.ps1 selftest OK"
    exit 0
}

if (-not [string]::IsNullOrWhiteSpace($DriverNameMapJson)) {
    $script:DriverNameMap = Load-DriverNameMap -Path $DriverNameMapJson
    if ($script:DriverNameMap.Count -gt 0) {
        $driverNameMapResolved = Resolve-RepoPath -Path $DriverNameMapJson
        Write-Host ("Loaded driver name overrides: {0} entry(s) from {1}" -f $script:DriverNameMap.Count, $driverNameMapResolved)
    }
}

if ($DeterminismSelfTest) {
    Invoke-DeterminismSelfTest -InputRoot $InputRoot -SigningPolicy $SigningPolicy -CertPath $CertPath -RepoRoot "." -Version $Version
    exit 0
}

$null = Invoke-PackageDrivers -InputRoot $InputRoot -SigningPolicy $SigningPolicy -CertPath $CertPath -OutDir $OutDir -RepoRoot "." -Version $Version -NoIso:$NoIso -MakeFatImage:$MakeFatImage -FatImageStrict:$FatImageStrict -FatImageSizeMB $FatImageSizeMB -NoManifest:$NoManifest -AllowUnsafeOutDir:$AllowUnsafeOutDir
