[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [Parameter(Mandatory = $true)]
  [string] $StagingDir,

  [string[]] $InfPaths,

  [string] $RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path,

  [string] $ToolchainJson,

  [string] $StampInfPath,

  [string] $DriverVerVersion,

  [DateTime] $DriverVerDate,

  [string] $PackageVersion
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-FullPath {
  param([Parameter(Mandatory = $true)][string] $Path)
  return (Resolve-Path -LiteralPath $Path).Path
}

function Resolve-ToolchainJsonPath {
  param([Parameter(Mandatory = $true)][string] $Path)

  $p = $Path
  if (-not [System.IO.Path]::IsPathRooted($p)) {
    # Prefer resolving relative paths against the repository root so callers can run this
    # script from any working directory (CI steps, sub-shells, etc).
    $p = Join-Path -Path $RepoRoot -ChildPath $p
  }
  if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
    throw "ToolchainJson not found: $Path (resolved to $p)"
  }
  return (Resolve-Path -LiteralPath $p).Path
}

function Read-StampInfPathFromToolchainJson {
  param([Parameter(Mandatory = $true)][string] $ToolchainJsonPath)

  $jsonPath = Resolve-ToolchainJsonPath -Path $ToolchainJsonPath
  $data = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json

  foreach ($propName in @('StampInfExe', 'StampinfExe', 'stampinfExe', 'stampInfExe')) {
    $prop = $data.PSObject.Properties[$propName]
    if ($null -ne $prop -and -not [string]::IsNullOrWhiteSpace([string]$prop.Value)) {
      return [string]$prop.Value
    }
  }

  return $null
}

function Get-WdkToolPath {
  param(
    [Parameter(Mandatory = $true)][string] $ToolName,
    [string] $ExplicitPath
  )

  if ($ExplicitPath) {
    $resolved = Resolve-FullPath $ExplicitPath
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
      throw "Tool path '$ExplicitPath' does not exist."
    }
    return $resolved
  }

  $cmd = Get-Command $ToolName -ErrorAction SilentlyContinue
  if ($cmd) {
    return $cmd.Source
  }

  $candidates = @()

  if ($env:ProgramFiles -and $env:"ProgramFiles(x86)") {
    $kits10 = Join-Path $env:"ProgramFiles(x86)" "Windows Kits\\10\\bin\\*\\x86\\$ToolName"
    $kits81 = Join-Path $env:"ProgramFiles(x86)" "Windows Kits\\8.1\\bin\\x86\\$ToolName"
    $candidates += Get-ChildItem -Path $kits10 -ErrorAction SilentlyContinue
    $candidates += Get-ChildItem -Path $kits81 -ErrorAction SilentlyContinue
  }

  if ($env:WDKContentRoot) {
    $candidates += Get-ChildItem -LiteralPath $env:WDKContentRoot -Recurse -Filter $ToolName -File -ErrorAction SilentlyContinue
  }

  if ($candidates.Count -eq 0) {
    throw "Unable to locate $ToolName. Install the Windows WDK or add it to PATH, or pass -StampInfPath."
  }

  $withParsedVersion = $candidates | ForEach-Object {
    $toolDir = Split-Path -Parent $_.FullName
    $parentDir = Split-Path -Parent $toolDir
    $maybeVersion = Split-Path -Leaf $parentDir
    $maybeParentVersion = Split-Path -Leaf (Split-Path -Parent $parentDir)

    $parsed = $null
    foreach ($v in @($maybeVersion, $maybeParentVersion)) {
      try {
        $parsed = [Version] $v
        break
      } catch {
        $parsed = $null
      }
    }
    if (-not $parsed) {
      $parsed = [Version] "0.0.0.0"
    }
    [pscustomobject]@{ Path = $_.FullName; Version = $parsed }
  }

  return ($withParsedVersion | Sort-Object Version -Descending | Select-Object -First 1).Path
}

function Invoke-Git {
  param([Parameter(Mandatory = $true)][string[]] $Args)

  $git = Get-Command git -ErrorAction SilentlyContinue
  if (-not $git) {
    throw "git was not found on PATH; required for version/date derivation. Provide -DriverVerVersion/-DriverVerDate overrides if building from a source tarball."
  }

  $result = & $git.Source -C $RepoRoot @Args 2>&1
  if ($LASTEXITCODE -ne 0) {
    throw "git $($Args -join ' ') failed: $result"
  }
  return ($result | Out-String).Trim()
}

function Get-DriverVerLine {
  param([Parameter(Mandatory = $true)][string] $InfPath)

  $content = Get-Content -LiteralPath $InfPath -Raw
  $m = [Regex]::Match($content, "(?im)^\\s*DriverVer\\s*=\\s*([^,\\r\\n]+)\\s*,\\s*([^\\s\\r\\n]+)")
  if (-not $m.Success) {
    return $null
  }
  return [pscustomobject]@{
    Date = $m.Groups[1].Value.Trim()
    Version = $m.Groups[2].Value.Trim()
    Raw = ("DriverVer={0},{1}" -f $m.Groups[1].Value.Trim(), $m.Groups[2].Value.Trim())
  }
}

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

function Get-VersionInfoFromGit {
  $shortSha = Invoke-Git @("rev-parse", "--short=7", "HEAD")
  $commitDateIso = Invoke-Git @("show", "-s", "--format=%cI", "HEAD")
  $commitDate = [DateTimeOffset]::Parse($commitDateIso, [System.Globalization.CultureInfo]::InvariantCulture)

  $tag = $null
  try {
    $tag = Invoke-Git @("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*")
  } catch {
    try {
      $tag = Invoke-Git @("describe", "--tags", "--abbrev=0")
    } catch {
      $tag = $null
    }
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
      $distance = [int] (Invoke-Git @("rev-list", "--count", "$tag..HEAD"))
    } catch {
      $distance = [int] (Invoke-Git @("rev-list", "--count", "HEAD"))
    }
  } else {
    $distance = [int] (Invoke-Git @("rev-list", "--count", "HEAD"))
  }

  $pkgVer = $null
  if ($distance -eq 0) {
    $pkgVer = "{0}+g{1}" -f $base.Text, $shortSha
  } else {
    $pkgVer = "{0}+{1}.g{2}" -f $base.Text, $distance, $shortSha
  }

  $infVer = "{0}.{1}.{2}.{3}" -f $base.Major, $base.Minor, $base.Patch, $distance
  return [pscustomobject]@{
    PackageVersion = $pkgVer
    InfVersion = $infVer
    CommitDate = $commitDate
    ShortSha = $shortSha
    BaseTag = $tag
  }
}

$stagingDirFull = Resolve-FullPath $StagingDir
if (-not (Test-Path -LiteralPath $stagingDirFull -PathType Container)) {
  throw "StagingDir '$StagingDir' does not exist or is not a directory."
}

$stagingDirWithSep = $stagingDirFull.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar

$resolvedInfPaths = @()
if ($InfPaths -and $InfPaths.Count -gt 0) {
  foreach ($p in $InfPaths) {
    $full = Resolve-FullPath $p
    if (-not $full.StartsWith($stagingDirWithSep, [StringComparison]::OrdinalIgnoreCase)) {
      throw "INF path '$full' is outside StagingDir '$stagingDirFull'. This script must only stamp INFs in the staging folder."
    }
    $resolvedInfPaths += $full
  }
} else {
  $resolvedInfPaths = Get-ChildItem -LiteralPath $stagingDirFull -Recurse -Filter "*.inf" -File | Select-Object -ExpandProperty FullName
}

if ($resolvedInfPaths.Count -eq 0) {
  Write-Host "No .inf files found in '$stagingDirFull'; nothing to stamp."
  exit 0
}

$needsGit = (-not $PackageVersion) -or (-not $DriverVerVersion) -or (-not $DriverVerDate)
$versionInfo = $null
if ($needsGit) {
  $versionInfo = Get-VersionInfoFromGit
}

$effectivePackageVersion = $PackageVersion
if (-not $effectivePackageVersion) {
  $effectivePackageVersion = $versionInfo.PackageVersion
}

$effectiveDriverVerVersion = $DriverVerVersion
if (-not $effectiveDriverVerVersion) {
  $effectiveDriverVerVersion = $versionInfo.InfVersion
}

if (-not ($effectiveDriverVerVersion -match "^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$")) {
  throw "DriverVer version '$effectiveDriverVerVersion' is not a 4-part numeric version (a.b.c.d)."
}

foreach ($part in ($effectiveDriverVerVersion.Split(".") | ForEach-Object { [int] $_ })) {
  if ($part -gt 65535) {
    throw "DriverVer version '$effectiveDriverVerVersion' contains component '$part' > 65535, which is not supported by Windows driver versioning."
  }
}

$buildTime = [DateTimeOffset]::Now
$effectiveDate = $null
if ($DriverVerDate) {
  $effectiveDate = [DateTimeOffset] $DriverVerDate
} else {
  $effectiveDate = $versionInfo.CommitDate
}

if ($effectiveDate -gt $buildTime) {
  Write-Warning "Requested stamp date '$effectiveDate' is in the future relative to build time '$buildTime'; clamping to build time."
  $effectiveDate = $buildTime
}

$dateString = $effectiveDate.ToString("MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture)

if (-not $StampInfPath -and $ToolchainJson) {
  $maybeStampInf = Read-StampInfPathFromToolchainJson -ToolchainJsonPath $ToolchainJson
  if ($maybeStampInf) {
    $StampInfPath = $maybeStampInf
  }
}

$stampInfExe = Get-WdkToolPath -ToolName "stampinf.exe" -ExplicitPath $StampInfPath

Write-Host "Stamping $($resolvedInfPaths.Count) INF(s) in '$stagingDirFull' with DriverVer=$dateString,$effectiveDriverVerVersion (package version $effectivePackageVersion)"

foreach ($inf in $resolvedInfPaths) {
  if ($PSCmdlet.ShouldProcess($inf, "stamp DriverVer")) {
    $stampOutput = & $stampInfExe -f $inf -d $dateString -v $effectiveDriverVerVersion 2>&1
    if ($LASTEXITCODE -ne 0) {
      throw "stampinf.exe failed for '$inf': $stampOutput"
    }
    if ($stampOutput) {
      Write-Verbose ($stampOutput | Out-String)
    }
  }

  $driverVer = Get-DriverVerLine -InfPath $inf
  if (-not $driverVer) {
    throw "After stamping, failed to locate DriverVer in '$inf'."
  }

  $expectedDate = [DateTime]::ParseExact($dateString, "MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture)
  $actualDate = [DateTime]::Parse($driverVer.Date, [System.Globalization.CultureInfo]::InvariantCulture)

  if (($actualDate.Date -ne $expectedDate.Date) -or ($driverVer.Version -ne $effectiveDriverVerVersion)) {
    throw "After stamping '$inf', observed '$($driverVer.Raw)' but expected DriverVer=$dateString,$effectiveDriverVerVersion."
  }

  Write-Host ("Stamped {0}: {1}" -f $inf, $driverVer.Raw)
}

[pscustomobject]@{
  PackageVersion = $effectivePackageVersion
  DriverVerDate = $dateString
  DriverVerVersion = $effectiveDriverVerVersion
  InfsStamped = $resolvedInfPaths
}
