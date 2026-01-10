Set-StrictMode -Version Latest

function ConvertTo-NormalizedOsList {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string[]] $OsList
  )

  $normalized = @()
  foreach ($entry in $OsList) {
    if ($null -eq $entry) { continue }
    foreach ($part in ($entry -split ',')) {
      $part = $part.Trim()
      if ($part.Length -gt 0) {
        $normalized += $part
      }
    }
  }

  if ($normalized.Count -eq 0) {
    throw "OsList is empty."
  }

  return $normalized
}

function Split-OsListByArchitecture {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string[]] $OsList
  )

  $byArch = @{
    x86 = @()
    x64 = @()
  }

  foreach ($os in (ConvertTo-NormalizedOsList -OsList $OsList)) {
    if ($os -match '_X86$') {
      $byArch.x86 += $os
      continue
    }
    if ($os -match '_X64$') {
      $byArch.x64 += $os
      continue
    }

    throw "Unsupported OS id '$os'. Expected *_X86 or *_X64 (examples: 7_X86, 7_X64, Server2008R2_X64)."
  }

  return $byArch
}

function Get-InfArchitectureSupport {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string] $InfPath
  )

  if (-not (Test-Path -LiteralPath $InfPath)) {
    throw "INF not found: $InfPath"
  }

  $content = Get-Content -LiteralPath $InfPath -Raw

  $hasX86 = $content -match '(?i)\bNTx86\b'
  $hasX64 = $content -match '(?i)\bNTamd64\b'

  if ($hasX86 -and $hasX64) {
    return 'both'
  }
  if ($hasX86) {
    return 'x86'
  }
  if ($hasX64) {
    return 'x64'
  }

  # Some INFs omit NTx86/NTamd64 decorations; treat as architecture-agnostic.
  return 'both'
}

function Resolve-Inf2CatPathFromToolchainJson {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string] $ToolchainJson
  )

  $jsonPath = $ToolchainJson
  if (-not [System.IO.Path]::IsPathRooted($jsonPath)) {
    $jsonPath = Join-Path -Path (Get-Location) -ChildPath $jsonPath
  }

  if (-not (Test-Path -LiteralPath $jsonPath)) {
    throw "ToolchainJson not found: $jsonPath"
  }

  $baseDir = Split-Path -Parent $jsonPath
  $data = Get-Content -LiteralPath $jsonPath -Raw | ConvertFrom-Json

  function Find-CandidateInf2CatPaths {
    param([object] $Node)

    $found = @()

    if ($null -eq $Node) { return $found }

    if ($Node -is [string]) {
      $found += $Node
      return $found
    }

    if ($Node -is [System.Collections.IDictionary]) {
      foreach ($value in $Node.Values) {
        $found += Find-CandidateInf2CatPaths -Node $value
      }
      return $found
    }

    if ($Node -is [System.Collections.IEnumerable] -and -not ($Node -is [string])) {
      foreach ($value in $Node) {
        $found += Find-CandidateInf2CatPaths -Node $value
      }
      return $found
    }

    foreach ($prop in ($Node | Get-Member -MemberType NoteProperty)) {
      $found += Find-CandidateInf2CatPaths -Node $Node.$($prop.Name)
    }

    return $found
  }

  $candidates = @()

  foreach ($propName in @('Inf2Cat', 'Inf2CatPath', 'inf2cat', 'inf2catPath')) {
    if ($null -ne ($data.PSObject.Properties[$propName])) {
      $candidates += $data.$propName
    }
  }

  $candidates += Find-CandidateInf2CatPaths -Node $data

  foreach ($candidate in $candidates) {
    if (-not ($candidate -is [string])) { continue }
    $candidate = [Environment]::ExpandEnvironmentVariables($candidate).Trim()
    if ($candidate.Length -eq 0) { continue }

    $path = $candidate
    if (-not [System.IO.Path]::IsPathRooted($path)) {
      $path = Join-Path -Path $baseDir -ChildPath $path
    }

    if ((Test-Path -LiteralPath $path) -and (Get-Item -LiteralPath $path).PSIsContainer) {
      $exe = Join-Path -Path $path -ChildPath 'Inf2Cat.exe'
      if (Test-Path -LiteralPath $exe) { return $exe }
      $exe = Join-Path -Path $path -ChildPath 'inf2cat.exe'
      if (Test-Path -LiteralPath $exe) { return $exe }
      continue
    }

    if ((Test-Path -LiteralPath $path) -and ($path -match '(?i)inf2cat\.exe$')) {
      return $path
    }
  }

  throw "Unable to locate Inf2Cat.exe via ToolchainJson ($jsonPath). Provide a JSON field like `\"Inf2Cat\": \"C:\\...\\Inf2Cat.exe\"`."
}

function Resolve-Inf2CatPath {
  [CmdletBinding()]
  param(
    [string] $ToolchainJson
  )

  if ($ToolchainJson) {
    return Resolve-Inf2CatPathFromToolchainJson -ToolchainJson $ToolchainJson
  }

  $cmd = Get-Command -Name 'Inf2Cat.exe' -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }

  $kitsRoots = @()
  if ($env:ProgramFiles -and ${env:ProgramFiles(x86)}) {
    $kitsRoots += Join-Path -Path ${env:ProgramFiles(x86)} -ChildPath 'Windows Kits'
  }

  foreach ($kitsRoot in $kitsRoots) {
    foreach ($kitsVersion in @('10', '8.1', '8.0')) {
      $binRoot = Join-Path -Path $kitsRoot -ChildPath "$kitsVersion\\bin"
      if (-not (Test-Path -LiteralPath $binRoot)) { continue }

      $versions = Get-ChildItem -LiteralPath $binRoot -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name -Descending
      foreach ($versionDir in $versions) {
        $exe = Join-Path -Path $versionDir.FullName -ChildPath 'x86\\Inf2Cat.exe'
        if (Test-Path -LiteralPath $exe) { return $exe }
      }

      $exe = Join-Path -Path $binRoot -ChildPath 'x86\\Inf2Cat.exe'
      if (Test-Path -LiteralPath $exe) { return $exe }
    }
  }

  throw "Inf2Cat.exe not found. Install the Windows Driver Kit or pass -ToolchainJson with an Inf2Cat path."
}

function Resolve-DriverBuildOutputDir {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string] $DriverBuildDir,

    [Parameter(Mandatory)]
    [ValidateSet('x86', 'x64')]
    [string] $Arch,

    [Parameter(Mandatory)]
    [string[]] $OsListForArch
  )

  if (-not (Test-Path -LiteralPath $DriverBuildDir)) {
    throw "Driver build dir not found: $DriverBuildDir"
  }

  $candidateNames = @()
  $candidateNames += $OsListForArch

  if ($Arch -eq 'x86') {
    $candidateNames += @('x86', 'X86', 'Win32', 'win32', 'i386', 'I386')
  } else {
    $candidateNames += @('x64', 'X64', 'amd64', 'AMD64', 'x86_64', 'X86_64')
  }

  foreach ($name in $candidateNames) {
    $p = Join-Path -Path $DriverBuildDir -ChildPath $name
    if (Test-Path -LiteralPath $p) {
      $item = Get-Item -LiteralPath $p
      if ($item.PSIsContainer) {
        return $item.FullName
      }
    }
  }

  # Fallback: if the driver build dir itself looks like a staged package (contains a SYS), use it.
  $directSys = Get-ChildItem -LiteralPath $DriverBuildDir -File -Filter '*.sys' -ErrorAction SilentlyContinue
  if ($directSys) {
    return $DriverBuildDir
  }

  $sysFiles = Get-ChildItem -LiteralPath $DriverBuildDir -Recurse -File -Filter '*.sys' -ErrorAction SilentlyContinue
  if (-not $sysFiles) {
    throw "Unable to locate any .sys files under $DriverBuildDir (needed for $Arch catalog generation)."
  }

  function Score-SysCandidate {
    param(
      [Parameter(Mandatory)]
      [System.IO.FileInfo] $File,
      [Parameter(Mandatory)]
      [string] $Arch
    )

    $path = $File.FullName.ToLowerInvariant()
    $score = 0

    if ($Arch -eq 'x86') {
      if ($path -match '(^|[\\\\/])x86([\\\\/]|$)') { $score += 10 }
      if ($path -match '(^|[\\\\/])win32([\\\\/]|$)') { $score += 5 }
      if ($path -match '(^|[\\\\/])i386([\\\\/]|$)') { $score += 5 }
      if ($path -match '_x86([\\\\/]|$)') { $score += 3 }
      if ($path -match 'amd64|x64|x86_64') { $score -= 10 }
    } else {
      if ($path -match '(^|[\\\\/])(x64|amd64|x86_64)([\\\\/]|$)') { $score += 10 }
      if ($path -match '_x64([\\\\/]|$)') { $score += 3 }
      if ($path -match 'x86([\\\\/]|$)') { $score -= 5 }
    }

    return $score
  }

  $best = $null
  $bestScore = [int]::MinValue
  foreach ($sys in $sysFiles) {
    $score = Score-SysCandidate -File $sys -Arch $Arch
    if ($score -gt $bestScore) {
      $bestScore = $score
      $best = $sys
    }
  }

  if (-not $best) {
    throw "Unable to determine a $Arch build output directory under $DriverBuildDir."
  }

  return (Split-Path -Parent $best.FullName)
}

function Invoke-Inf2Cat {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)]
    [string] $Inf2CatPath,

    [Parameter(Mandatory)]
    [string] $PackageDir,

    [Parameter(Mandatory)]
    [string[]] $OsList
  )

  if (-not (Test-Path -LiteralPath $Inf2CatPath)) {
    throw "Inf2Cat.exe not found: $Inf2CatPath"
  }

  if (-not (Test-Path -LiteralPath $PackageDir)) {
    throw "Package dir not found: $PackageDir"
  }

  $osArg = (ConvertTo-NormalizedOsList -OsList $OsList) -join ','

  # Inf2Cat generates catalog file names based on the CatalogFile= directive inside INF(s).
  # Ensure we don't accidentally reuse stale catalogs.
  Get-ChildItem -LiteralPath $PackageDir -Filter '*.cat' -File -Recurse -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

  $stdoutFile = [System.IO.Path]::GetTempFileName()
  $stderrFile = [System.IO.Path]::GetTempFileName()

  try {
    $argString = "/driver:`"$PackageDir`" /os:$osArg /verbose"

    $proc = Start-Process `
      -FilePath $Inf2CatPath `
      -ArgumentList $argString `
      -WorkingDirectory $PackageDir `
      -NoNewWindow `
      -Wait `
      -PassThru `
      -RedirectStandardOutput $stdoutFile `
      -RedirectStandardError $stderrFile

    $exitCode = $proc.ExitCode

    $cats = Get-ChildItem -LiteralPath $PackageDir -Filter '*.cat' -File -Recurse -ErrorAction SilentlyContinue

    if ($exitCode -ne 0 -or -not $cats) {
      $stdout = Get-Content -LiteralPath $stdoutFile -Raw -ErrorAction SilentlyContinue
      $stderr = Get-Content -LiteralPath $stderrFile -Raw -ErrorAction SilentlyContinue

      $logFiles = Get-ChildItem -LiteralPath $PackageDir -File -Recurse -Filter '*.log' -ErrorAction SilentlyContinue
      $logText = @()
      foreach ($log in $logFiles) {
        $logText += "----- $($log.FullName) -----"
        $logText += (Get-Content -LiteralPath $log.FullName -Raw -ErrorAction SilentlyContinue)
      }

      $details = @()
      if ($stdout) { $details += "Inf2Cat stdout:`n$stdout" }
      if ($stderr) { $details += "Inf2Cat stderr:`n$stderr" }
      if ($logText) { $details += "Inf2Cat logs:`n$($logText -join \"`n\")" }

      if (-not $details) {
        $details = @("No Inf2Cat output or logs were captured.")
      }

      if ($exitCode -eq 0 -and -not $cats) {
        throw "Inf2Cat completed but produced no .cat files in $PackageDir.`n$($details -join \"`n`n\")"
      }

      throw "Inf2Cat failed (exit code $exitCode) for $PackageDir.`n$($details -join \"`n`n\")"
    }
  } finally {
    Remove-Item -LiteralPath $stdoutFile -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stderrFile -Force -ErrorAction SilentlyContinue
  }
}

Export-ModuleMember -Function @(
  'ConvertTo-NormalizedOsList',
  'Split-OsListByArchitecture',
  'Get-InfArchitectureSupport',
  'Resolve-Inf2CatPath',
  'Resolve-DriverBuildOutputDir',
  'Invoke-Inf2Cat'
)
