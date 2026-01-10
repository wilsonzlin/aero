Set-StrictMode -Version Latest

function Write-ToolchainLog {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Message,
    [ValidateSet('INFO', 'WARN', 'ERROR')]
    [string]$Level = 'INFO'
  )

  $timestamp = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ss.fffK')
  Write-Host "[$timestamp] [$Level] $Message"
}

function Resolve-ExistingPath {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$LiteralPath
  )

  return (Resolve-Path -LiteralPath $LiteralPath).Path
}

function Get-VsWhereExe {
  [CmdletBinding()]
  param()

  $candidate = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  if (Test-Path -LiteralPath $candidate) {
    return (Resolve-ExistingPath -LiteralPath $candidate)
  }

  return $null
}

function Get-VsInstallationPath {
  [CmdletBinding()]
  param()

  $vswhere = Get-VsWhereExe
  if ($null -ne $vswhere) {
    $installPath = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -property installationPath 2>$null
    $installPath = [string]$installPath
    $installPath = $installPath.Trim()
    if (-not [string]::IsNullOrWhiteSpace($installPath) -and (Test-Path -LiteralPath $installPath)) {
      return (Resolve-ExistingPath -LiteralPath $installPath)
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($env:VSINSTALLDIR) -and (Test-Path -LiteralPath $env:VSINSTALLDIR)) {
    return (Resolve-ExistingPath -LiteralPath $env:VSINSTALLDIR)
  }

  return $null
}

function Get-VsDevCmdBat {
  [CmdletBinding()]
  param()

  $installPath = Get-VsInstallationPath
  if ($null -eq $installPath) {
    return $null
  }

  $candidate = Join-Path $installPath 'Common7\Tools\VsDevCmd.bat'
  if (Test-Path -LiteralPath $candidate) {
    return (Resolve-ExistingPath -LiteralPath $candidate)
  }

  return $null
}

function Get-VcVarsAllBat {
  [CmdletBinding()]
  param()

  $installPath = Get-VsInstallationPath
  if ($null -eq $installPath) {
    return $null
  }

  $candidate = Join-Path $installPath 'VC\Auxiliary\Build\vcvarsall.bat'
  if (Test-Path -LiteralPath $candidate) {
    return (Resolve-ExistingPath -LiteralPath $candidate)
  }

  return $null
}

function Get-MSBuildExe {
  [CmdletBinding()]
  param()

  $vswhere = Get-VsWhereExe
  if ($null -ne $vswhere) {
    $findPatterns = @(
      'MSBuild\Current\Bin\amd64\MSBuild.exe',
      'MSBuild\Current\Bin\MSBuild.exe',
      'MSBuild\**\Bin\amd64\MSBuild.exe',
      'MSBuild\**\Bin\MSBuild.exe'
    )

    foreach ($pattern in $findPatterns) {
      $paths = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find $pattern 2>$null
      foreach ($path in @($paths)) {
        if (-not [string]::IsNullOrWhiteSpace($path) -and (Test-Path -LiteralPath $path)) {
          return (Resolve-ExistingPath -LiteralPath $path)
        }
      }
    }
  }

  $cmd = Get-Command msbuild.exe -ErrorAction SilentlyContinue
  if ($null -ne $cmd -and -not [string]::IsNullOrWhiteSpace($cmd.Source) -and (Test-Path -LiteralPath $cmd.Source)) {
    return (Resolve-ExistingPath -LiteralPath $cmd.Source)
  }

  throw @"
msbuild.exe was not found.

Remediation:
  - Install Visual Studio 2022 or the Visual Studio 2022 Build Tools (MSBuild).
  - Verify that vswhere.exe is available and that MSBuild is installed.
"@
}

function ConvertTo-VersionSafe {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$VersionText
  )

  try {
    return [Version]$VersionText
  } catch {
    return $null
  }
}

function Get-WindowsKitsRoot {
  [CmdletBinding()]
  param()

  $regRoots = @(
    'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots'
  )

  foreach ($regRoot in $regRoots) {
    try {
      $props = Get-ItemProperty -Path $regRoot -ErrorAction Stop
      foreach ($propName in @('KitsRoot10', 'KitsRoot81')) {
        $value = $props.PSObject.Properties[$propName].Value
        if ([string]::IsNullOrWhiteSpace($value)) {
          continue
        }

        $kitRoot = [string]$value
        $kitRoot = $kitRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
        if ([string]::IsNullOrWhiteSpace($kitRoot)) {
          continue
        }

        $base = Split-Path -Parent $kitRoot
        if (-not [string]::IsNullOrWhiteSpace($base) -and (Test-Path -LiteralPath $base)) {
          return (Resolve-ExistingPath -LiteralPath $base)
        }
      }
    } catch {
      # ignore and continue
    }
  }

  foreach ($pf in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
    if ([string]::IsNullOrWhiteSpace($pf)) {
      continue
    }

    $candidate = Join-Path $pf 'Windows Kits'
    if (Test-Path -LiteralPath $candidate) {
      return (Resolve-ExistingPath -LiteralPath $candidate)
    }
  }

  return $null
}

function Test-Inf2CatSupportsWin7 {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Inf2CatExe
  )

  if (-not (Test-Path -LiteralPath $Inf2CatExe)) {
    return $false
  }

  try {
    $help = & $Inf2CatExe '/?' 2>&1 | Out-String
  } catch {
    Write-ToolchainLog -Level WARN -Message "Failed to execute Inf2Cat.exe to verify Windows 7 support: $($_.Exception.Message)"
    return $false
  }

  return ($help -match '\b7_X86\b' -and $help -match '\b7_X64\b')
}

function Get-WindowsKitVersionBins {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$BinRoot
  )

  $bins = @()

  if (-not (Test-Path -LiteralPath $BinRoot)) {
    return $bins
  }

  $versionDirs =
    Get-ChildItem -LiteralPath $BinRoot -Directory -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
      Sort-Object { [Version]$_.Name } -Descending

  foreach ($dir in $versionDirs) {
    $bins += [pscustomobject]@{
      Version = [Version]$dir.Name
      Path    = $dir.FullName
      Source  = 'versioned'
    }
  }

  # Some installations expose tools under bin\x64, bin\x86 without a version folder.
  $bins += [pscustomobject]@{
    Version = [Version]'0.0.0.0'
    Path    = $BinRoot
    Source  = 'unversioned'
  }

  return $bins
}

function Find-KitTool {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$BinDir,
    [Parameter(Mandatory = $true)]
    [string]$ToolName,
    [string[]]$Architectures = @('x64', 'x86')
  )

  foreach ($arch in $Architectures) {
    $candidate = Join-Path $BinDir (Join-Path $arch $ToolName)
    if (Test-Path -LiteralPath $candidate) {
      return (Resolve-ExistingPath -LiteralPath $candidate)
    }
  }

  return $null
}

function Resolve-WindowsKitTool {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$ToolName,
    [string[]]$Architectures = @('x64', 'x86'),
    [string[]]$KitVersions = @('10', '8.1'),
    [switch]$RequireWin7Inf2Cat
  )

  $kitsRoot = Get-WindowsKitsRoot
  if ([string]::IsNullOrWhiteSpace($kitsRoot) -or -not (Test-Path -LiteralPath $kitsRoot)) {
    return $null
  }

  foreach ($kitVersion in $KitVersions) {
    $binRoot = Join-Path $kitsRoot (Join-Path $kitVersion 'bin')
    if (-not (Test-Path -LiteralPath $binRoot)) {
      continue
    }

    $bins = Get-WindowsKitVersionBins -BinRoot $binRoot
    foreach ($bin in $bins) {
      $exe = Find-KitTool -BinDir $bin.Path -ToolName $ToolName -Architectures $Architectures
      if ($null -eq $exe) {
        continue
      }

      if ($RequireWin7Inf2Cat -and -not (Test-Inf2CatSupportsWin7 -Inf2CatExe $exe)) {
        Write-ToolchainLog -Level WARN -Message "Ignoring Windows Kit candidate (tool=$ToolName, kit=$kitVersion, bin=$($bin.Path)) because it does not advertise 7_X86/7_X64 support."
        continue
      }

      return [pscustomobject]@{
        Exe = $exe
        KitFamily = $kitVersion
        KitBinDir = $bin.Path
        KitBinSource = $bin.Source
        KitToolVersion = $bin.Version.ToString()
      }
    }
  }

  return $null
}

function Resolve-WindowsKitToolchain {
  [CmdletBinding()]
  param(
    [Parameter()]
    [string[]]$KitVersions = @('10', '8.1'),
    [Parameter()]
    [switch]$RequireWin7Inf2Cat
  )

  $kitsRoot = Get-WindowsKitsRoot
  if ([string]::IsNullOrWhiteSpace($kitsRoot) -or -not (Test-Path -LiteralPath $kitsRoot)) {
    return $null
  }

  $inf2cat = Resolve-WindowsKitTool -ToolName 'Inf2Cat.exe' -Architectures @('x64', 'x86') -KitVersions $KitVersions -RequireWin7Inf2Cat:$RequireWin7Inf2Cat
  $signtool = Resolve-WindowsKitTool -ToolName 'signtool.exe' -Architectures @('x64', 'x86') -KitVersions $KitVersions
  $stampinf = Resolve-WindowsKitTool -ToolName 'stampinf.exe' -Architectures @('x64', 'x86') -KitVersions $KitVersions

  if ($null -eq $inf2cat -or $null -eq $signtool) {
    return $null
  }

  return [pscustomobject]@{
    Inf2CatExe = $inf2cat.Exe
    SignToolExe = $signtool.Exe
    StampInfExe = if ($null -ne $stampinf) { $stampinf.Exe } else { $null }
    WindowsKits = @{
      Inf2Cat = $inf2cat
      SignTool = $signtool
      StampInf = $stampinf
    }
  }
}

function Get-WingetExe {
  [CmdletBinding()]
  param()

  $cmd = Get-Command winget.exe -ErrorAction SilentlyContinue
  if ($null -ne $cmd -and -not [string]::IsNullOrWhiteSpace($cmd.Source) -and (Test-Path -LiteralPath $cmd.Source)) {
    return (Resolve-ExistingPath -LiteralPath $cmd.Source)
  }

  return $null
}

function Get-ChocoExe {
  [CmdletBinding()]
  param()

  $cmd = Get-Command choco.exe -ErrorAction SilentlyContinue
  if ($null -ne $cmd -and -not [string]::IsNullOrWhiteSpace($cmd.Source) -and (Test-Path -LiteralPath $cmd.Source)) {
    return (Resolve-ExistingPath -LiteralPath $cmd.Source)
  }

  $cmd = Get-Command choco -ErrorAction SilentlyContinue
  if ($null -ne $cmd -and -not [string]::IsNullOrWhiteSpace($cmd.Source) -and (Test-Path -LiteralPath $cmd.Source)) {
    return (Resolve-ExistingPath -LiteralPath $cmd.Source)
  }

  return $null
}

function Test-IsAdministrator {
  [CmdletBinding()]
  param()

  try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
  } catch {
    return $false
  }
}

function Invoke-ExternalCommand {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$FilePath,
    [Parameter(Mandatory = $true)]
    [string[]]$Arguments,
    [string]$FailureHint
  )

  $prettyArgs = ($Arguments | ForEach-Object {
    if ($_ -match '\s') { '"' + $_.Replace('"', '\"') + '"' } else { $_ }
  }) -join ' '

  Write-ToolchainLog -Message "Running: $FilePath $prettyArgs"

  $stdoutFile = [System.IO.Path]::GetTempFileName()
  $stderrFile = [System.IO.Path]::GetTempFileName()

  try {
    $proc = Start-Process `
      -FilePath $FilePath `
      -ArgumentList $Arguments `
      -NoNewWindow `
      -Wait `
      -PassThru `
      -RedirectStandardOutput $stdoutFile `
      -RedirectStandardError $stderrFile

    if ($proc.ExitCode -ne 0) {
      $stdout = Get-Content -LiteralPath $stdoutFile -Raw -ErrorAction SilentlyContinue
      $stderr = Get-Content -LiteralPath $stderrFile -Raw -ErrorAction SilentlyContinue

      $details = @()
      if (-not [string]::IsNullOrWhiteSpace($stdout)) { $details += "stdout:`n$stdout" }
      if (-not [string]::IsNullOrWhiteSpace($stderr)) { $details += "stderr:`n$stderr" }

      $hint = ''
      if (-not [string]::IsNullOrWhiteSpace($FailureHint)) {
        $hint = "`n`n$FailureHint"
      }

      $detailText = ''
      if ($details.Count -gt 0) {
        $detailText = "`n`n" + ($details -join "`n`n")
      }

      throw "Command failed with exit code $($proc.ExitCode): $FilePath $prettyArgs$detailText$hint"
    }
  } finally {
    Remove-Item -LiteralPath $stdoutFile -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stderrFile -Force -ErrorAction SilentlyContinue
  }
}

function Install-WingetPackage {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$WingetId,
    [string]$WingetVersion,
    [Parameter(Mandatory = $true)]
    [string]$DisplayName,
    [string]$DownloadDirectory
  )

  $winget = Get-WingetExe
  if ($null -eq $winget) {
    throw @"
winget.exe was not found, so $DisplayName cannot be installed automatically.

Remediation:
  - Install winget (App Installer) or install $DisplayName manually from Microsoft.
"@
  }

  $baseArgs = @(
    'install',
    '--id', $WingetId,
    '--exact',
    '--source', 'winget',
    '--accept-source-agreements',
    '--accept-package-agreements',
    '--silent'
  )

  $downloadDirFull = $null
  if (-not [string]::IsNullOrWhiteSpace($DownloadDirectory)) {
    $downloadDirFull = $DownloadDirectory
    if (-not [System.IO.Path]::IsPathRooted($downloadDirFull)) {
      $downloadDirFull = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $downloadDirFull))
    }

    if (-not (Test-Path -LiteralPath $downloadDirFull)) {
      New-Item -ItemType Directory -Force -Path $downloadDirFull | Out-Null
    }

    $baseArgs += @('--download-directory', $downloadDirFull)
  }

  $flagSets = @(
    @('--disable-interactivity', '--force'),
    @('--disable-interactivity'),
    @('--force'),
    @()
  )

  if (-not [string]::IsNullOrWhiteSpace($WingetVersion)) {
    $baseArgs += @('--version', $WingetVersion)
  }

  foreach ($flags in $flagSets) {
    $args = @($baseArgs + $flags)
    try {
      Invoke-ExternalCommand -FilePath $winget -Arguments $args -FailureHint @"
If this keeps failing on a CI runner, check whether the winget package ID/version has changed.
You can inspect available versions with:
  winget show --id $WingetId --versions
"@
      return
    } catch {
      $message = $_.Exception.Message

      $unknownForce = ($flags -contains '--force') -and ($message -match '(?i)(unknown|unrecognized).*(--force)')
      $unknownDisable = ($flags -contains '--disable-interactivity') -and ($message -match '(?i)(unknown|unrecognized).*(--disable-interactivity)')
      $unknownDownloadDir = ($baseArgs -contains '--download-directory') -and ($message -match '(?i)(unknown|unrecognized).*(--download-directory)')

      # If the failure is clearly due to an unsupported flag, try the next reduced flag set.
      if ($unknownForce -or $unknownDisable -or $unknownDownloadDir) {
        Write-ToolchainLog -Level WARN -Message "winget does not support one or more flags ($($flags -join ' ')); retrying with fewer flags."
        if ($unknownDownloadDir) {
          Write-ToolchainLog -Level WARN -Message 'winget does not support --download-directory; continuing without download caching.'
          $baseArgs = $baseArgs | Where-Object { $_ -ne '--download-directory' -and $_ -ne $downloadDirFull }
          $downloadDirFull = $null
        }
        continue
      }

      throw
    }
  }

  throw "winget install failed for $WingetId with all supported flag combinations."
}

function Install-WdkViaChocolatey {
  [CmdletBinding()]
  param()

  $choco = Get-ChocoExe
  if ($null -eq $choco) {
    throw 'choco.exe was not found.'
  }

  $args = @(
    'install',
    'windows-driver-kit',
    '-y',
    '--no-progress'
  )

  Invoke-ExternalCommand -FilePath $choco -Arguments $args -FailureHint @"
If this is a CI runner and Chocolatey package installation is flaky, prefer using winget by installing the Windows SDK/WDK manually.
"@
}

function Ensure-WindowsKitToolchain {
  [CmdletBinding()]
  param(
    [Parameter()]
    [string]$PreferredWdkWingetId = 'Microsoft.WindowsWDK',
    [Parameter()]
    [string]$PreferredWdkKitVersion = '10.0.22621.0'
  )

  $toolchain = Resolve-WindowsKitToolchain -RequireWin7Inf2Cat
  if ($null -ne $toolchain) {
    return $toolchain
  }

  $kitFamilyPreference = @('10', '8.1')
  $inf2cat = Resolve-WindowsKitTool -ToolName 'Inf2Cat.exe' -Architectures @('x64', 'x86') -KitVersions $kitFamilyPreference -RequireWin7Inf2Cat
  $signtool = Resolve-WindowsKitTool -ToolName 'signtool.exe' -Architectures @('x64', 'x86') -KitVersions $kitFamilyPreference

  $needsWdk = ($null -eq $inf2cat)
  $needsSdk = ($null -eq $signtool)

  $missing = @()
  if ($needsWdk) { $missing += 'Inf2Cat.exe (WDK)' }
  if ($needsSdk) { $missing += 'signtool.exe (Windows SDK)' }

  if (-not (Test-IsAdministrator)) {
    throw @"
Windows driver tooling is missing ($($missing -join ', ')), but this process is not running with Administrator privileges.

Remediation:
  - Re-run this script in an elevated PowerShell (Run as Administrator), or
  - Install the Windows SDK/WDK manually.
"@
  }

  Write-ToolchainLog -Level WARN -Message "Required Windows Kits tooling not found ($($missing -join ', ')). Attempting to install via winget (preferred) with Chocolatey fallback..."

  $winget = Get-WingetExe
  if ($null -eq $winget) {
    Write-ToolchainLog -Level WARN -Message 'winget.exe was not found. Falling back to Chocolatey (windows-driver-kit) if available...'
    try {
      Install-WdkViaChocolatey
    } catch {
      Write-ToolchainLog -Level WARN -Message "Chocolatey WDK install attempt failed: $($_.Exception.Message)"
    }

    $toolchain = Resolve-WindowsKitToolchain -RequireWin7Inf2Cat
    if ($null -ne $toolchain) {
      return $toolchain
    }
  }

  # winget versions for SDK/WDK installers do not always match the installed Kit version exactly.
  $versionCandidates = @(
    $PreferredWdkKitVersion,
    ($PreferredWdkKitVersion -replace '^10\.0\.', '10.1.'),
    ((ConvertTo-VersionSafe -VersionText $PreferredWdkKitVersion) | ForEach-Object { if ($_ -ne $null) { "10.1.$($_.Build).1" } })
  ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
  # Last resort: install whatever winget considers "latest" if we can't match the pinned version string.
  $versionCandidates += $null

  $installAttempted = $false
  $wdkIdCandidates = @(
    $PreferredWdkWingetId,
    'Microsoft.WindowsDriverKit'
  ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
  $sdkIdCandidates = @('Microsoft.WindowsSDK')

  foreach ($versionCandidate in $versionCandidates) {
    $versionLabel = if ([string]::IsNullOrWhiteSpace($versionCandidate)) { 'latest' } else { $versionCandidate }

    if ($needsSdk) {
      foreach ($sdkId in $sdkIdCandidates) {
        $installAttempted = $true
        try {
          Write-ToolchainLog -Message "Installing Windows SDK via winget (id=$sdkId, version=$versionLabel)..."
          Install-WingetPackage -WingetId $sdkId -WingetVersion $versionCandidate -DisplayName 'Windows SDK' -DownloadDirectory $env:WDK_DOWNLOAD_CACHE
          break
        } catch {
          Write-ToolchainLog -Level WARN -Message "Windows SDK install attempt failed (id=$sdkId, version=$versionLabel): $($_.Exception.Message)"
        }
      }
    }

    if ($needsWdk) {
      foreach ($wdkId in $wdkIdCandidates) {
        $installAttempted = $true
        try {
          Write-ToolchainLog -Message "Installing Windows Driver Kit via winget (id=$wdkId, version=$versionLabel)..."
          Install-WingetPackage -WingetId $wdkId -WingetVersion $versionCandidate -DisplayName 'Windows Driver Kit (WDK)' -DownloadDirectory $env:WDK_DOWNLOAD_CACHE
          break
        } catch {
          Write-ToolchainLog -Level WARN -Message "WDK install attempt failed (id=$wdkId, version=$versionLabel): $($_.Exception.Message)"
        }
      }
    }

    $toolchain = Resolve-WindowsKitToolchain -RequireWin7Inf2Cat
    if ($null -ne $toolchain) {
      return $toolchain
    }

    $needsWdk = ($null -eq (Resolve-WindowsKitTool -ToolName 'Inf2Cat.exe' -Architectures @('x64', 'x86') -KitVersions $kitFamilyPreference -RequireWin7Inf2Cat))
    $needsSdk = ($null -eq (Resolve-WindowsKitTool -ToolName 'signtool.exe' -Architectures @('x64', 'x86') -KitVersions $kitFamilyPreference))
    if (-not $needsWdk -and -not $needsSdk) {
      break
    }
  }

  if (-not $installAttempted) {
    throw 'No toolchain installation attempt was made; version candidates list was empty.'
  }

  $choco = Get-ChocoExe
  if ($null -ne $choco) {
    Write-ToolchainLog -Level WARN -Message 'Toolchain is still missing after winget attempts; trying Chocolatey (windows-driver-kit) as a fallback...'
    try {
      Install-WdkViaChocolatey
      $toolchain = Resolve-WindowsKitToolchain -RequireWin7Inf2Cat
      if ($null -ne $toolchain) {
        return $toolchain
      }
    } catch {
      Write-ToolchainLog -Level WARN -Message "Chocolatey WDK install attempt failed: $($_.Exception.Message)"
    }
  }

  throw @"
Windows driver toolchain tooling is still missing after installation attempts.

Expected tools:
  - Inf2Cat.exe (WDK; must support /os:7_X86,7_X64)
  - signtool.exe (Windows SDK)

Remediation:
  1. Install the Windows SDK and WDK manually and ensure they install under:
       ${env:ProgramFiles(x86)}\Windows Kits\10
  2. Re-run: pwsh -File ci/install-wdk.ps1

If you have multiple Windows Kits installed, this script selects the newest versioned bin directory for each tool (Inf2Cat.exe, signtool.exe, stampinf.exe).
"@
}

function Add-PathEntry {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string]$Directory
  )

  if ([string]::IsNullOrWhiteSpace($Directory) -or -not (Test-Path -LiteralPath $Directory)) {
    return
  }

  $current = $env:PATH
  $segments = @()
  if (-not [string]::IsNullOrWhiteSpace($current)) {
    $segments = $current -split ';'
  }

  if ($segments -contains $Directory) {
    return
  }

  $env:PATH = "$Directory;$current"
}

function Publish-ToolchainToGitHubActions {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [pscustomobject]$Toolchain
  )

  $outputs = @{
    'toolchain_json' = $Toolchain.ToolchainJson
    'msbuild_exe'  = $Toolchain.MSBuildExe
    'inf2cat_exe'  = $Toolchain.Inf2CatExe
    'signtool_exe' = $Toolchain.SignToolExe
    'stampinf_exe' = $Toolchain.StampInfExe
  }

  if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_OUTPUT)) {
    foreach ($key in $outputs.Keys) {
      $val = $outputs[$key]
      if ($null -ne $val -and -not [string]::IsNullOrWhiteSpace($val)) {
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "$key=$val"
      }
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_ENV)) {
    foreach ($key in $outputs.Keys) {
      $envKey = $key.ToUpperInvariant()
      $val = $outputs[$key]
      if ($null -ne $val -and -not [string]::IsNullOrWhiteSpace($val)) {
        Add-Content -LiteralPath $env:GITHUB_ENV -Value "$envKey=$val"
      }
    }
  }

  if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_PATH)) {
    $dirs = @(
      (Split-Path -Path $Toolchain.MSBuildExe -Parent),
      (Split-Path -Path $Toolchain.Inf2CatExe -Parent),
      (Split-Path -Path $Toolchain.SignToolExe -Parent)
    )

    if (-not [string]::IsNullOrWhiteSpace($Toolchain.StampInfExe)) {
      $dirs += (Split-Path -Path $Toolchain.StampInfExe -Parent)
    }

    foreach ($dir in ($dirs | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique)) {
      Add-Content -LiteralPath $env:GITHUB_PATH -Value $dir
    }
  }
}

Export-ModuleMember -Function `
  Write-ToolchainLog, `
  Get-VsDevCmdBat, `
  Get-VcVarsAllBat, `
  Get-MSBuildExe, `
  Ensure-WindowsKitToolchain, `
  Add-PathEntry, `
  Publish-ToolchainToGitHubActions
