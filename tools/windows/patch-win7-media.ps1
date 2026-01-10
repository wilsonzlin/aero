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
    "CA" { return "CA" }
    "TRUSTEDPUBLISHER" { return "TrustedPublisher" }
    "TRUSTEDPEOPLE" { return "TrustedPeople" }
    default { throw "Unsupported certificate store '$Name'. Supported values: ROOT, CA, TrustedPublisher, TrustedPeople." }
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

function Get-BcdBootLoaderEntries {
  param([Parameter(Mandatory)][string]$StorePath)

  # Use /v so we can see `device` / `osdevice` for filtering (e.g. to only patch the boot.wim WinPE loader).
  $output = Invoke-NativeCommandWithOutput -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/enum", "all", "/v")

  $sectionTitle = $null
  $current = $null
  $entries = New-Object System.Collections.Generic.List[object]

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
      if ($current) {
        $entries.Add([pscustomobject]$current)
        $current = $null
      }
      $sectionTitle = $trimmed
      continue
    }

    if ($sectionTitle -ne "Windows Boot Loader") {
      continue
    }

    if ($trimmed -match '^identifier\s+(\{[^}]+\})\s*$') {
      $current = @{
        Identifier = $Matches[1]
        Device = $null
        OsDevice = $null
      }
      continue
    }

    if (-not $current) {
      continue
    }

    if ($trimmed -match '^device\s+(.+)$') {
      $current.Device = $Matches[1].Trim()
      continue
    }
    if ($trimmed -match '^osdevice\s+(.+)$') {
      $current.OsDevice = $Matches[1].Trim()
      continue
    }
  }

  if ($current) {
    $entries.Add([pscustomobject]$current)
  }

  return $entries
}

function Get-BcdBootLoaderIdentifiers {
  param([Parameter(Mandatory)][string]$StorePath)

  return @(
    (Get-BcdBootLoaderEntries -StorePath $StorePath | Select-Object -ExpandProperty Identifier -Unique)
  )
}

function Get-BcdBootLoaderIdentifiersForBootWim {
  param([Parameter(Mandatory)][string]$StorePath)

  $entries = Get-BcdBootLoaderEntries -StorePath $StorePath
  $bootWimEntries = $entries | Where-Object {
    ($_.Device -and $_.Device -match '(?i)\\sources\\boot\.wim') -or
    ($_.OsDevice -and $_.OsDevice -match '(?i)\\sources\\boot\.wim')
  }

  return @($bootWimEntries | Select-Object -ExpandProperty Identifier -Unique)
}

function Get-BcdGuidsFromStore {
  param([Parameter(Mandatory)][string]$StorePath)

  $output = Invoke-NativeCommandWithOutput -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/enum", "all", "/v")

  # Avoid locale-specific parsing of bcdedit output by extracting GUID-shaped identifiers directly.
  $guids = New-Object System.Collections.Generic.List[string]
  foreach ($line in $output) {
    foreach ($match in [regex]::Matches($line, '\{[0-9A-Fa-f\-]{36}\}')) {
      $g = $match.Value
      if (-not ($guids -contains $g)) {
        $guids.Add($g) | Out-Null
      }
    }
  }

  return $guids.ToArray()
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
    [switch]$EnableNoIntegrityChecks,

    # If set, only patches Windows Boot Loader objects whose `device`/`osdevice` references \sources\boot.wim.
    # This prevents unintentionally modifying unrelated loader objects on customized/OEM media.
    [switch]$OnlyBootWimLoaders
  )

  if (-not (Test-Path -LiteralPath $StorePath -PathType Leaf)) {
    throw "BCD store not found ($StoreLabel): $StorePath"
  }

  Write-Host "`n[$StoreLabel] Patching BCD store: $StorePath"
  Invoke-NativeCommand -FilePath "attrib.exe" -ArgumentList @("-h", "-s", "-r", $StorePath) -SuppressOutput

  $targets = @()
  try {
    if ($OnlyBootWimLoaders) {
      $targets = Get-BcdBootLoaderIdentifiersForBootWim -StorePath $StorePath
    }
    else {
      $targets = Get-BcdBootLoaderIdentifiers -StorePath $StorePath
    }
  }
  catch {
    # If bcdedit output parsing fails (for example due to localization), fall back to other strategies below.
    Write-Warning "[$StoreLabel] Failed to enumerate Windows Boot Loader entries via bcdedit output parsing. Falling back."
    $targets = @()
  }

  if ($targets.Count -gt 0) {
    foreach ($id in $targets) {
      Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $id, "testsigning", "on")
      if ($EnableNoIntegrityChecks) {
        Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $id, "nointegritychecks", "on")
      }
    }

    Write-Host "Verification hint:"
    Write-Host ("  bcdedit /store {0} /enum all /v" -f (Format-Arg $StorePath))
    return
  }

  Write-Warning "[$StoreLabel] Unable to locate Windows Boot Loader entries to patch. Falling back to {default}."

  $defaultResult = Invoke-NativeCommandResult -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", "{default}", "testsigning", "on") -SuppressOutput
  if ($defaultResult.ExitCode -eq 0) {
    if ($EnableNoIntegrityChecks) {
      Invoke-NativeCommand -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", "{default}", "nointegritychecks", "on")
    }

    Write-Host "Verification hint:"
    Write-Host ("  bcdedit /store {0} /enum all /v" -f (Format-Arg $StorePath))
    return
  }

  Write-Warning "[$StoreLabel] Failed to patch {default} in this store. Attempting GUID-based patching instead."
  $outputText = ($defaultResult.Output | Out-String).Trim()
  if ($outputText) {
    Write-Warning $outputText
  }

  $guids = @()
  if ($OnlyBootWimLoaders) {
    try {
      $output = Invoke-NativeCommandWithOutput -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/enum", "all", "/v")
      $currentGuid = $null
      $sectionHasBootWim = $false
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

        if ($nextLine -and ($nextLine -match '^-+$')) {
          if ($currentGuid -and $sectionHasBootWim -and (-not ($guids -contains $currentGuid))) {
            $guids += $currentGuid
          }
          $currentGuid = $null
          $sectionHasBootWim = $false
          continue
        }

        if (-not $currentGuid) {
          $m = [regex]::Match($line, '\{[0-9A-Fa-f\-]{36}\}')
          if ($m.Success) {
            $currentGuid = $m.Value
          }
        }

        if ($line -match '(?i)\\sources\\boot\.wim') {
          $sectionHasBootWim = $true
        }
      }
      if ($currentGuid -and $sectionHasBootWim -and (-not ($guids -contains $currentGuid))) {
        $guids += $currentGuid
      }
    }
    catch {
      $guids = @()
    }
  }

  if ($guids.Count -eq 0) {
    $guids = Get-BcdGuidsFromStore -StorePath $StorePath
  }
  if ($guids.Count -eq 0) {
    throw "Unable to locate any GUIDs in '$StorePath'. Run: bcdedit /store $StorePath /enum all"
  }

  $patched = 0
  $noIntegrityPatched = 0
  foreach ($guid in $guids) {
    $setResult = Invoke-NativeCommandResult -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $guid, "testsigning", "on") -SuppressOutput
    if ($setResult.ExitCode -ne 0) {
      continue
    }

    $patched++
    if ($EnableNoIntegrityChecks) {
      $nicResult = Invoke-NativeCommandResult -FilePath "bcdedit.exe" -ArgumentList @("/store", $StorePath, "/set", $guid, "nointegritychecks", "on") -SuppressOutput
      if ($nicResult.ExitCode -eq 0) {
        $noIntegrityPatched++
      }
    }
  }

  if ($patched -eq 0) {
    throw "Unable to patch testsigning in '$StorePath'. bcdedit failed for {default} and for all GUID entries found in /enum all output."
  }
  if ($EnableNoIntegrityChecks -and $noIntegrityPatched -eq 0) {
    throw "Unable to patch nointegritychecks in '$StorePath'. testsigning was applied to $patched entry(ies), but nointegritychecks failed for all GUID entries."
  }

  Write-Host "Verification hint:"
  Write-Host ("  bcdedit /store {0} /enum all /v" -f (Format-Arg $StorePath))
}

function Find-MediaBcdStores {
  param([Parameter(Mandatory)][string]$MediaRoot)

  $stores = New-Object System.Collections.Generic.List[object]

  $biosCandidates = @(
    [System.IO.Path]::Combine($MediaRoot, "boot", "BCD"),
    [System.IO.Path]::Combine($MediaRoot, "Boot", "BCD")
  )

  $biosPath = $null
  foreach ($p in $biosCandidates) {
    if (Test-Path -LiteralPath $p -PathType Leaf) {
      $biosPath = $p
      break
    }
  }
  if (-not $biosPath) {
    throw "Expected BIOS BCD store at 'boot\\BCD' under '$MediaRoot'."
  }
  $stores.Add([pscustomobject]@{ Label = "Media BIOS"; Path = $biosPath })

  $uefiCandidates = @(
    [System.IO.Path]::Combine($MediaRoot, "efi", "microsoft", "boot", "bcd"),
    [System.IO.Path]::Combine($MediaRoot, "EFI", "Microsoft", "Boot", "BCD"),
    [System.IO.Path]::Combine($MediaRoot, "EFI", "Boot", "BCD"),
    [System.IO.Path]::Combine($MediaRoot, "EFI", "BOOT", "BCD")
  )

  foreach ($p in ($uefiCandidates | Sort-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $p -PathType Leaf)) {
      continue
    }
    if ($stores | Where-Object { $_.Path -ieq $p }) {
      continue
    }
    $rel = $p.Substring($MediaRoot.Length).TrimStart('\', '/')
    $stores.Add([pscustomobject]@{ Label = "Media UEFI ($rel)"; Path = $p })
  }

  # Some OEM/custom layouts may place the UEFI BCD elsewhere under EFI/. Scan for additional stores.
  $efiRoot = [System.IO.Path]::Combine($MediaRoot, "EFI")
  if (Test-Path -LiteralPath $efiRoot -PathType Container) {
    $found = Get-ChildItem -LiteralPath $efiRoot -Recurse -Force -File -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -ieq "BCD" }
    foreach ($f in $found) {
      $full = $f.FullName
      if ($stores | Where-Object { $_.Path -ieq $full }) {
        continue
      }
      $rel = $full.Substring($MediaRoot.Length).TrimStart('\', '/')
      $stores.Add([pscustomobject]@{ Label = "Media UEFI ($rel)"; Path = $full })
    }
  }

  return @($stores)
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

  # Inject using CryptoAPI so the registry-backed store entry (including the `Blob` bytes) matches
  # what Windows would create on a live system.
  $injectArgs = @("--hive", $softwareHive)
  foreach ($storeName in $Stores) {
    $injectArgs += @("--store", $storeName)
  }
  $injectArgs += @("--cert", $CertPath)
  Invoke-NativeCommand -FilePath $injector -ArgumentList $injectArgs

  # Validate the offline hive contains the expected keys and a non-empty Blob.
  if (Test-Path "HKLM:\OFFLINE_SOFTWARE") {
    throw "HKLM:\OFFLINE_SOFTWARE already exists. Another offline hive may already be loaded. Unload it (reg unload HKLM\OFFLINE_SOFTWARE) or reboot, then retry."
  }

  $cert = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($CertPath)
  $thumb = $cert.Thumbprint.ToUpperInvariant()

  $loaded = $false
  $hadError = $false
  try {
    Invoke-NativeCommand -FilePath "reg.exe" -SuppressOutput -ArgumentList @("load", "HKLM\OFFLINE_SOFTWARE", $softwareHive)
    $loaded = $true

    foreach ($storeName in $Stores) {
      $keyPath = "HKLM:\OFFLINE_SOFTWARE\Microsoft\SystemCertificates\$storeName\Certificates\$thumb"
      if (-not (Test-Path -LiteralPath $keyPath)) {
        throw "Offline certificate injection failed validation for store '$storeName' (thumbprint $thumb): key not found: $keyPath"
      }

      $blob = (Get-ItemProperty -LiteralPath $keyPath -Name "Blob" -ErrorAction Stop).Blob
      if ($null -eq $blob -or $blob.Length -eq 0) {
        throw "Offline certificate injection failed validation for store '$storeName' (thumbprint $thumb): missing/empty Blob value."
      }
    }
  }
  catch {
    $hadError = $true
    throw
  }
  finally {
    if ($loaded) {
      try {
        Invoke-NativeCommand -FilePath "reg.exe" -SuppressOutput -ArgumentList @("unload", "HKLM\OFFLINE_SOFTWARE")
      }
      catch {
        Write-Warning "Failed to unload HKLM\OFFLINE_SOFTWARE. You may need to run: reg unload HKLM\OFFLINE_SOFTWARE"
        if (-not $hadError) {
          throw
        }
      }
    }
  }
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
    [Parameter(Mandatory)][string]$CertPath,
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
        -CertPath $CertPath `
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
          -CertPath $CertPath `
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
Assert-CommandAvailable -Name "reg.exe"
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

$mediaBcdStores = Find-MediaBcdStores -MediaRoot $resolvedMediaRoot

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
$hasUefiStore = $false
foreach ($store in $mediaBcdStores) {
  Write-Host ("  - {0}: {1}" -f $store.Label, $store.Path)
  if ($store.Label -like "Media UEFI*") {
    $hasUefiStore = $true
  }
}
if (-not $hasUefiStore) {
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
    Set-BcdFlagsForStore -StorePath $store.Path -StoreLabel $store.Label -EnableNoIntegrityChecks:$EnableNoIntegrityChecks -OnlyBootWimLoaders
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
