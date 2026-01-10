#Requires -Version 5.1

[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [string]$MediaRoot,

  [Parameter(Mandatory)]
  [string]$CertPath,

  # Certificate stores to populate in the offline SOFTWARE hive.
  # Default matches the minimum needed for trusting test-signed kernel-mode driver catalogs.
  [string[]]$CertStores = @("ROOT", "TrustedPublisher"),

  [string]$DriversPath,

  [int[]]$BootWimIndices = @(1, 2),

  # Accepts "all" or a comma-separated list (e.g. "1,4,5").
  [string]$InstallWimIndices = "all",

  # Optionally patch the nested Windows Recovery Environment image inside each install.wim index:
  #   Windows\System32\Recovery\winre.wim
  [switch]$PatchNestedWinRE,

  [switch]$EnableNoIntegrityChecks
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-IsAdministrator {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated PowerShell (Run as Administrator)."
  }
}

function Assert-CommandAvailable {
  param([Parameter(Mandatory)][string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command '$Name' was not found in PATH."
  }
}

function Normalize-CertStoreName {
  param([Parameter(Mandatory)][string]$Name)

  $upper = $Name.Trim().ToUpperInvariant()
  switch ($upper) {
    "ROOT" { return "ROOT" }
    "TRUSTEDPUBLISHER" { return "TrustedPublisher" }
    "TRUSTEDPEOPLE" { return "TrustedPeople" }
    default { throw "Unsupported certificate store '$Name'. Supported values: ROOT, TrustedPublisher, TrustedPeople." }
  }
}

function Normalize-CertStoreList {
  param([Parameter(Mandatory)][string[]]$Stores)

  $out = New-Object System.Collections.Generic.List[string]
  foreach ($store in $Stores) {
    if ([string]::IsNullOrWhiteSpace($store)) {
      continue
    }
    $norm = Normalize-CertStoreName -Name $store
    if (-not ($out -contains $norm)) {
      $out.Add($norm) | Out-Null
    }
  }

  if ($out.Count -eq 0) {
    throw "-CertStores must contain at least one store."
  }

  return $out.ToArray()
}

function Ensure-WritableFile {
  param(
    [Parameter(Mandatory)][string]$Path,
    [Parameter(Mandatory)][string]$Label
  )

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "$Label not found: $Path"
  }

  # ISO extractors commonly mark files read-only; clear it so DISM can commit modifications.
  Invoke-NativeCommand -FilePath "attrib.exe" -ArgumentList @("-r", $Path) -SuppressOutput

  $item = Get-Item -LiteralPath $Path -ErrorAction Stop
  if ($item.Attributes -band [System.IO.FileAttributes]::ReadOnly) {
    throw "$Label is read-only and cannot be serviced in-place. Copy the extracted ISO contents to a writable NTFS directory and retry. Path: $Path"
  }
}

function Format-Arg {
  param([Parameter(Mandatory)][string]$Arg)
  if ($Arg -match '[\s"`]') {
    return '"' + ($Arg -replace '"', '\"') + '"'
  }
  return $Arg
}

function Invoke-NativeCommandResult {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $cmdLine = ("{0} {1}" -f $FilePath, (($ArgumentList | ForEach-Object { Format-Arg $_ }) -join " ")).Trim()
  Write-Host "`n> $cmdLine"

  $output = & $FilePath @ArgumentList 2>&1

  if (-not $SuppressOutput) {
    foreach ($line in $output) {
      Write-Host $line
    }
  }

  return [pscustomobject]@{
    ExitCode = $LASTEXITCODE
    Output = ,$output
    CommandLine = $cmdLine
  }
}

function Invoke-NativeCommand {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList,
    [switch]$SuppressOutput
  )

  $result = Invoke-NativeCommandResult -FilePath $FilePath -ArgumentList $ArgumentList -SuppressOutput:$SuppressOutput
  if ($result.ExitCode -ne 0) {
    $outputText = ($result.Output | Out-String).Trim()
    if ($outputText) {
      throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)`n`n$outputText"
    }
    throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)"
  }
}

function Invoke-NativeCommandWithOutput {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][string]$FilePath,
    [Parameter(Mandatory)][string[]]$ArgumentList
  )

  $result = Invoke-NativeCommandResult -FilePath $FilePath -ArgumentList $ArgumentList -SuppressOutput
  if ($result.ExitCode -ne 0) {
    $outputText = ($result.Output | Out-String).Trim()
    if ($outputText) {
      throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)`n`n$outputText"
    }
    throw "Command failed with exit code $($result.ExitCode): $($result.CommandLine)"
  }

  return ,$result.Output
}

function Get-BcdBootLoaderGuids {
  param([Parameter(Mandatory)][string]$StorePath)

  $output = Invoke-NativeCommandWithOutput -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/enum", "all")

  $sectionTitle = $null
  $guids = @()
  for ($i = 0; $i -lt $output.Count; $i++) {
    $line = $output[$i]
    $trimmed = $line.Trim()
    if (-not $trimmed) {
      continue
    }

    $nextLine = $null
    if ($i + 1 -lt $output.Count) {
      $nextLine = $output[$i + 1].Trim()
    }

    # bcdedit section headers are immediately followed by a dashed separator line.
    if ($nextLine -and ($nextLine -match '^-+$')) {
      $sectionTitle = $trimmed
      continue
    }

    if ($sectionTitle -eq "Windows Boot Loader" -and ($trimmed -match '^identifier\s+(\{[0-9A-Fa-f\-]{36}\})\s*$')) {
      $guids += $Matches[1]
    }
  }

  return @($guids | Sort-Object -Unique)
}

function Get-WimIndexList {
  param([Parameter(Mandatory)][string]$WimFile)

  if (-not (Test-Path -LiteralPath $WimFile -PathType Leaf)) {
    throw "WIM file not found: $WimFile"
  }

  $output = Invoke-NativeCommandWithOutput -FilePath "dism.exe" -ArgumentList @(
    "/English",
    "/Get-WimInfo",
    ("/WimFile:$WimFile")
  )

  $indices = @()
  foreach ($line in $output) {
    if ($line -match '^\s*Index\s*:\s*(\d+)\s*$') {
      $indices += [int]$Matches[1]
    }
  }

  if ($indices.Count -eq 0) {
    $raw = ($output | Out-String).Trim()
    throw "Failed to parse WIM indices from DISM output for '$WimFile'. Output was:`n`n$raw"
  }

  return @($indices | Sort-Object -Unique)
}

function Parse-InstallWimIndexSelection {
  param(
    [Parameter(Mandatory)][string]$Selection,
    [Parameter(Mandatory)][int[]]$AvailableIndices
  )

  $trimmed = $Selection.Trim()
  if ($trimmed.ToLowerInvariant() -eq "all") {
    return @($AvailableIndices | Sort-Object -Unique)
  }

  $parsed = @()
  foreach ($part in ($trimmed -split ",")) {
    $p = $part.Trim()
    if (-not $p) {
      continue
    }

    $idx = 0
    if (-not [int]::TryParse($p, [ref]$idx)) {
      throw "Invalid value in -InstallWimIndices: '$p'. Expected 'all' or a comma-separated list like '1,4,5'."
    }
    $parsed += $idx
  }

  $parsed = @($parsed | Sort-Object -Unique)
  if ($parsed.Count -eq 0) {
    throw "No install.wim indices were selected. Expected 'all' or a comma-separated list like '1,4,5'."
  }

  foreach ($idx in $parsed) {
    if ($AvailableIndices -notcontains $idx) {
      throw "install.wim index $idx does not exist. Available indices: $($AvailableIndices -join ', ')"
    }
  }

  return $parsed
}

function Assert-SelectedWimIndicesExist {
  param(
    [Parameter(Mandatory)][string]$WimLabel,
    [Parameter(Mandatory)][int[]]$Selected,
    [Parameter(Mandatory)][int[]]$Available
  )

  foreach ($idx in $Selected) {
    if ($Available -notcontains $idx) {
      throw "$WimLabel index $idx does not exist. Available indices: $($Available -join ', ')"
    }
  }
}

function Set-BcdFlagsForStore {
  param(
    [Parameter(Mandatory)][string]$StorePath,
    [Parameter(Mandatory)][string]$StoreLabel,
    [switch]$EnableNoIntegrityChecks
  )

  if (-not (Test-Path -LiteralPath $StorePath -PathType Leaf)) {
    throw "BCD store not found ($StoreLabel): $StorePath"
  }

  Write-Host "`n[$StoreLabel] Patching BCD store: $StorePath"
  Invoke-NativeCommand -FilePath "attrib.exe" -ArgumentList @("-h", "-s", "-r", $StorePath) -SuppressOutput

  $defaultResult = Invoke-NativeCommandResult -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", "{default}", "testsigning", "on")
  if ($defaultResult.ExitCode -eq 0) {
    if ($EnableNoIntegrityChecks) {
      Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", "{default}", "nointegritychecks", "on")
    }

    Write-Host "Verification hint:"
    Write-Host ("  bcdedit /store {0} /enum {{default}}" -f (Format-Arg $StorePath))
    return
  }

  Write-Warning "[$StoreLabel] Failed to patch {default} in this store. Attempting to patch Windows Boot Loader entries instead."
  $outputText = ($defaultResult.Output | Out-String).Trim()
  if ($outputText) {
    Write-Warning $outputText
  }

  $bootLoaderGuids = Get-BcdBootLoaderGuids -StorePath $StorePath
  if ($bootLoaderGuids.Count -eq 0) {
    throw "Unable to locate any Windows Boot Loader GUIDs in '$StorePath'. Run: bcdedit /store $StorePath /enum all"
  }

  foreach ($guid in $bootLoaderGuids) {
    Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $guid, "testsigning", "on")
    if ($EnableNoIntegrityChecks) {
      Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $guid, "nointegritychecks", "on")
    }
  }

  Write-Host "Verification hint:"
  Write-Host ("  bcdedit /store {0} /enum all" -f (Format-Arg $StorePath))
}

function Add-OfflineTrustedCertificate {
  param(
    [Parameter(Mandatory)][string]$MountedImageRoot,
    [Parameter(Mandatory)][string]$CertPath,
    [Parameter(Mandatory)][string[]]$Stores
  )

  $softwareHive = [System.IO.Path]::Combine($MountedImageRoot, "Windows", "System32", "Config", "SOFTWARE")
  if (-not (Test-Path -LiteralPath $softwareHive -PathType Leaf)) {
    throw "Offline SOFTWARE hive not found at '$softwareHive'. Is this a Windows image?"
  }

  $repoRoot = (Resolve-Path -LiteralPath (Join-Path -Path $PSScriptRoot -ChildPath "..\\..")).Path
  $candidates = @(
    (Join-Path -Path $repoRoot -ChildPath "tools\\win-offline-cert-injector\\target\\release\\win-offline-cert-injector.exe"),
    (Join-Path -Path $repoRoot -ChildPath "tools\\win-offline-cert-injector\\target\\debug\\win-offline-cert-injector.exe")
  )
  $injector = $null
  foreach ($p in $candidates) {
    if (Test-Path -LiteralPath $p -PathType Leaf) {
      $injector = (Resolve-Path -LiteralPath $p).Path
      break
    }
  }
  if (-not $injector) {
    $cmd = Get-Command "win-offline-cert-injector.exe" -ErrorAction SilentlyContinue
    if ($cmd) {
      $injector = $cmd.Source
    }
  }
  if (-not $injector) {
    throw ("win-offline-cert-injector.exe not found. Build it with:`n`n" +
      "  cd tools\\win-offline-cert-injector`n" +
      "  cargo build --release`n`n" +
      "Then re-run this script.")
  }

  $args = @("--hive", $softwareHive)
  foreach ($storeName in $Stores) {
    $args += "--store"
    $args += $storeName
  }
  $args += $CertPath

  Invoke-NativeCommand -FilePath $injector -ArgumentList $args
}

function Add-DriversToOfflineImage {
  param(
    [Parameter(Mandatory)][string]$MountedImageRoot,
    [Parameter(Mandatory)][string]$DriversRoot
  )

  Write-Host "`nInjecting drivers from: $DriversRoot"
  Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList @(
    "/Image:$MountedImageRoot",
    "/Add-Driver",
    "/Driver:$DriversRoot",
    "/Recurse"
  )
}

function Patch-InstallBcdTemplate {
  param(
    [Parameter(Mandatory)][string]$MountedImageRoot,
    [switch]$EnableNoIntegrityChecks
  )

  $templatePath = [System.IO.Path]::Combine($MountedImageRoot, "Windows", "System32", "Config", "BCD-Template")
  if (-not (Test-Path -LiteralPath $templatePath -PathType Leaf)) {
    throw "install.wim BCD-Template not found at '$templatePath'."
  }

  Write-Host "`nPatching install.wim BCD template: $templatePath"
  Set-BcdFlagsForStore -StorePath $templatePath -StoreLabel "install.wim BCD-Template" -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
}

function Service-NestedWinreWim {
  param(
    [Parameter(Mandatory)][string]$MountedInstallImageRoot,
    [Parameter(Mandatory)][string]$TempRoot,
    [Parameter(Mandatory)][string]$ParentLabel,
    [Parameter(Mandatory)][System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate,
    [Parameter(Mandatory)][string[]]$CertStores,
    [string]$DriversRoot
  )

  $winrePath = [System.IO.Path]::Combine($MountedInstallImageRoot, "Windows", "System32", "Recovery", "winre.wim")
  if (-not (Test-Path -LiteralPath $winrePath -PathType Leaf)) {
    Write-Host "`n[$ParentLabel] Nested winre.wim not found (skipping): $winrePath"
    return
  }

  $workDir = Join-Path -Path $TempRoot -ChildPath ("nested-winre-" + [Guid]::NewGuid().ToString("N"))
  $winreCopy = Join-Path -Path $workDir -ChildPath "winre.wim"

  $succeeded = $false
  try {
    New-Item -ItemType Directory -Path $workDir | Out-Null

    Write-Host "`n[$ParentLabel] Servicing nested winre.wim: $winrePath"
    Copy-Item -LiteralPath $winrePath -Destination $winreCopy -Force
    Ensure-WritableFile -Path $winreCopy -Label "Nested winre.wim working copy"

    $winreIndices = Get-WimIndexList -WimFile $winreCopy
    foreach ($idx in $winreIndices) {
      $mountDir = Join-Path -Path $workDir -ChildPath ("mount-index-{0}" -f $idx)
      Service-WimIndex `
        -WimFile $winreCopy `
        -Index $idx `
        -MountDir $mountDir `
        -Label ("{0} nested winre.wim index {1}" -f $ParentLabel, $idx) `
        -Certificate $Certificate `
        -CertStores $CertStores `
        -DriversRoot $DriversRoot `
        -IsInstallImage:$false `
        -EnableNoIntegrityChecks:$false `
        -PatchNestedWinRE:$false `
        -TempRoot $TempRoot
    }

    Invoke-NativeCommand -FilePath "attrib.exe" -ArgumentList @("-h", "-s", "-r", $winrePath) -SuppressOutput
    Copy-Item -LiteralPath $winreCopy -Destination $winrePath -Force

    $succeeded = $true
  }
  finally {
    if ($succeeded) {
      try {
        if (Test-Path -LiteralPath $workDir) {
          Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction Stop
        }
      }
      catch {
        Write-Warning "[$ParentLabel] Failed to delete nested winre.wim work directory '$workDir'. You can remove it manually."
      }
    }
    else {
      Write-Warning "[$ParentLabel] Nested winre.wim servicing did not complete. Work directory kept for inspection: $workDir"
    }
  }
}

function Service-WimIndex {
  param(
    [Parameter(Mandatory)][string]$WimFile,
    [Parameter(Mandatory)][int]$Index,
    [Parameter(Mandatory)][string]$MountDir,
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][string]$CertPath,
    [Parameter(Mandatory)][string[]]$CertStores,
    [string]$DriversRoot,
    [switch]$IsInstallImage,
    [switch]$PatchNestedWinRE,
    [Parameter(Mandatory)][string]$TempRoot,
    [switch]$EnableNoIntegrityChecks
  )

  New-Item -ItemType Directory -Path $MountDir | Out-Null

  $mounted = $false
  $unmounted = $false
  $hadError = $false
  $shouldCommit = $false

  try {
    Write-Host "`n[$Label] Mounting WIM index $Index..."
    Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList @(
      "/Mount-Wim",
      ("/WimFile:$WimFile"),
      ("/Index:$Index"),
      ("/MountDir:$MountDir")
    )
    $mounted = $true

    if ($DriversRoot) {
      Add-DriversToOfflineImage -MountedImageRoot $MountDir -DriversRoot $DriversRoot
    }

    Write-Host "`n[$Label] Trusting certificate offline ($($CertStores -join ', '))..."
    Add-OfflineTrustedCertificate -MountedImageRoot $MountDir -CertPath $CertPath -Stores $CertStores

    if ($IsInstallImage) {
      Patch-InstallBcdTemplate -MountedImageRoot $MountDir -EnableNoIntegrityChecks:$EnableNoIntegrityChecks

      if ($PatchNestedWinRE) {
        Service-NestedWinreWim `
          -MountedInstallImageRoot $MountDir `
          -TempRoot $TempRoot `
          -ParentLabel $Label `
          -Certificate $Certificate `
          -CertStores $CertStores `
          -DriversRoot $DriversRoot
      }
    }

    $shouldCommit = $true
  }
  catch {
    $hadError = $true
    throw
  }
  finally {
    if ($mounted) {
      $unmountArgs = @("/Unmount-Wim", ("/MountDir:$MountDir"))
      if ($shouldCommit) {
        Write-Host "`n[$Label] Unmounting (commit)..."
        $unmountArgs += "/Commit"
      }
      else {
        Write-Warning "[$Label] Unmounting (discard) due to earlier failure..."
        $unmountArgs += "/Discard"
      }

      try {
        Invoke-NativeCommand -FilePath "dism.exe" -ArgumentList $unmountArgs
        $unmounted = $true
      }
      catch {
        if ($hadError) {
          Write-Warning "[$Label] DISM failed to unmount. The image may still be mounted at: $MountDir"
        }
        else {
          throw
        }
      }
    }

    if (-not $mounted -or $unmounted) {
      try {
        if (Test-Path -LiteralPath $MountDir) {
          Remove-Item -LiteralPath $MountDir -Recurse -Force -ErrorAction Stop
        }
      }
      catch {
        Write-Warning "[$Label] Failed to delete mount directory '$MountDir'. You can remove it manually after ensuring the image is unmounted."
        if (-not $hadError) {
          throw
        }
      }
    }
    elseif (Test-Path -LiteralPath $MountDir) {
      Write-Warning "[$Label] Keeping mount directory because the image may still be mounted: $MountDir"
    }
  }
}

Assert-IsAdministrator
Assert-CommandAvailable -Name "dism.exe"
Assert-CommandAvailable -Name "bcdedit.exe"
Assert-CommandAvailable -Name "attrib.exe"

if (-not (Test-Path -LiteralPath $MediaRoot -PathType Container)) {
  throw "-MediaRoot must be an existing directory. Got: $MediaRoot"
}
if (-not (Test-Path -LiteralPath $CertPath -PathType Leaf)) {
  throw "-CertPath must be an existing file. Got: $CertPath"
}
if ($DriversPath) {
  if (-not (Test-Path -LiteralPath $DriversPath -PathType Container)) {
    throw "-DriversPath must be an existing directory. Got: $DriversPath"
  }

  $infCount = @(Get-ChildItem -LiteralPath $DriversPath -Recurse -Filter "*.inf" -File -ErrorAction SilentlyContinue).Count
  if ($infCount -eq 0) {
    throw "-DriversPath '$DriversPath' does not contain any .inf files."
  }
}

$resolvedMediaRoot = (Resolve-Path -LiteralPath $MediaRoot).Path
$resolvedCertPath = (Resolve-Path -LiteralPath $CertPath).Path
$resolvedDriversPath = $null
if ($DriversPath) {
  $resolvedDriversPath = (Resolve-Path -LiteralPath $DriversPath).Path
}

$bootWimPath = [System.IO.Path]::Combine($resolvedMediaRoot, "sources", "boot.wim")
$installWimPath = [System.IO.Path]::Combine($resolvedMediaRoot, "sources", "install.wim")
if (-not (Test-Path -LiteralPath $bootWimPath -PathType Leaf)) {
  throw "Expected boot.wim at '$bootWimPath' (MediaRoot must contain 'sources\boot.wim')."
}
if (-not (Test-Path -LiteralPath $installWimPath -PathType Leaf)) {
  throw "Expected install.wim at '$installWimPath' (MediaRoot must contain 'sources\install.wim')."
}

# Ensure the media files are writable before we attempt to service them.
Ensure-WritableFile -Path $bootWimPath -Label "boot.wim"
Ensure-WritableFile -Path $installWimPath -Label "install.wim"

$biosBcdPath = [System.IO.Path]::Combine($resolvedMediaRoot, "boot", "BCD")
$uefiBcdPath = [System.IO.Path]::Combine($resolvedMediaRoot, "efi", "microsoft", "boot", "bcd")

$mediaBcdStores = @()
if (Test-Path -LiteralPath $biosBcdPath -PathType Leaf) {
  $mediaBcdStores += @{ Label = "Media BIOS"; Path = $biosBcdPath }
}
else {
  throw "Expected BIOS BCD store at '$biosBcdPath'."
}
if (Test-Path -LiteralPath $uefiBcdPath -PathType Leaf) {
  $mediaBcdStores += @{ Label = "Media UEFI"; Path = $uefiBcdPath }
}

$certificate = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($resolvedCertPath)
$thumbprint = $certificate.Thumbprint.ToUpperInvariant()
$normalizedCertStores = Normalize-CertStoreList -Stores $CertStores

$availableBootIndices = Get-WimIndexList -WimFile $bootWimPath
$availableInstallIndices = Get-WimIndexList -WimFile $installWimPath

$selectedBootIndices = @($BootWimIndices | Sort-Object -Unique)
if ($selectedBootIndices.Count -eq 0) {
  throw "No boot.wim indices were selected."
}
Assert-SelectedWimIndicesExist -WimLabel "boot.wim" -Selected $selectedBootIndices -Available $availableBootIndices

$selectedInstallIndices = Parse-InstallWimIndexSelection -Selection $InstallWimIndices -AvailableIndices $availableInstallIndices

$tempRoot = Join-Path -Path $env:TEMP -ChildPath ("patch-win7-media-" + [Guid]::NewGuid().ToString("N"))

Write-Host "========================================"
Write-Host "Windows 7 media patch plan"
Write-Host "========================================"
Write-Host "MediaRoot            : $resolvedMediaRoot"
Write-Host "CertPath             : $resolvedCertPath"
Write-Host "Cert thumbprint      : $thumbprint"
Write-Host "Cert stores          : $($normalizedCertStores -join ', ')"
Write-Host "DriversPath          : $(if ($resolvedDriversPath) { $resolvedDriversPath } else { "<none>" })"
Write-Host "Patch nested WinRE    : $(if ($PatchNestedWinRE) { "ON" } else { "OFF" })"
Write-Host "EnableNoIntegrityChecks : $(if ($EnableNoIntegrityChecks) { "ON" } else { "OFF" })"
Write-Host ""
Write-Host "boot.wim             : $bootWimPath"
Write-Host "  Available indices  : $($availableBootIndices -join ', ')"
Write-Host "  Selected indices   : $($selectedBootIndices -join ', ')"
Write-Host "install.wim          : $installWimPath"
Write-Host "  Available indices  : $($availableInstallIndices -join ', ')"
Write-Host "  Selected indices   : $($selectedInstallIndices -join ', ')"
Write-Host ""
Write-Host "Media BCD stores to patch:"
foreach ($store in $mediaBcdStores) {
  Write-Host ("  - {0}: {1}" -f $store.Label, $store.Path)
}
if (-not (Test-Path -LiteralPath $uefiBcdPath -PathType Leaf)) {
  Write-Host "  - Media UEFI: <not found> (skipping)"
}
Write-Host ""
Write-Host "Temp mount root      : $tempRoot"
Write-Host "========================================`n"

$scriptSucceeded = $false
try {
  New-Item -ItemType Directory -Path $tempRoot | Out-Null

  Write-Host "Step 1/3: Patching media BCD stores (outside WIMs)..."
  foreach ($store in $mediaBcdStores) {
    Set-BcdFlagsForStore -StorePath $store.Path -StoreLabel $store.Label -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
  }

  Write-Host "`nStep 2/3: Servicing boot.wim..."
  foreach ($idx in $selectedBootIndices) {
    $mountDir = Join-Path -Path $tempRoot -ChildPath ("boot-index-{0}" -f $idx)
    Service-WimIndex `
      -WimFile $bootWimPath `
      -Index $idx `
      -MountDir $mountDir `
      -Label ("boot.wim index $idx") `
      -CertPath $resolvedCertPath `
      -CertStores $normalizedCertStores `
      -DriversRoot $resolvedDriversPath `
      -IsInstallImage:$false `
      -PatchNestedWinRE:$false `
      -TempRoot $tempRoot `
      -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
  }

  Write-Host "`nStep 3/3: Servicing install.wim..."
  foreach ($idx in $selectedInstallIndices) {
    $mountDir = Join-Path -Path $tempRoot -ChildPath ("install-index-{0}" -f $idx)
    Service-WimIndex `
      -WimFile $installWimPath `
      -Index $idx `
      -MountDir $mountDir `
      -Label ("install.wim index $idx") `
      -CertPath $resolvedCertPath `
      -CertStores $normalizedCertStores `
      -DriversRoot $resolvedDriversPath `
      -IsInstallImage `
      -PatchNestedWinRE:$PatchNestedWinRE `
      -TempRoot $tempRoot `
      -EnableNoIntegrityChecks:$EnableNoIntegrityChecks
  }

  $scriptSucceeded = $true
  Write-Host "`nDone. Media is patched in-place under: $resolvedMediaRoot"
}
finally {
  if ($scriptSucceeded) {
    try {
      if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction Stop
      }
    }
    catch {
      Write-Warning "Failed to delete temp directory '$tempRoot'. You can remove it manually."
    }
  }
  else {
    Write-Warning "Script did not complete successfully. Temp directory kept for inspection: $tempRoot"
  }
}
