#Requires -Version 5.1

[CmdletBinding()]
param(
  [Parameter()]
  [string]$InputRoot = "out/packages",

  [Parameter()]
  [string]$CertOutDir = "out/certs",

  [Parameter()]
  [ValidateSet("sha1", "sha256")]
  [string]$Digest = "sha1",

  [Parameter()]
  [switch]$DualSign,

  [Parameter()]
  [string]$ToolchainJson
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-AbsolutePath {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path,

    [Parameter(Mandatory = $true)]
    [string]$BaseDir
  )

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }

  return [System.IO.Path]::GetFullPath((Join-Path $BaseDir $Path))
}

function Get-JsonPropertyValueRecursive {
  param(
    [Parameter(Mandatory = $true)]
    $Object,

    [Parameter(Mandatory = $true)]
    [string[]]$PropertyNames
  )

  if ($null -eq $Object) {
    return $null
  }

  if ($Object -is [string] -or $Object -is [ValueType]) {
    return $null
  }

  if ($Object -is [System.Collections.IDictionary]) {
    foreach ($key in $Object.Keys) {
      if ($PropertyNames -contains $key) {
        return $Object[$key]
      }

      $value = Get-JsonPropertyValueRecursive -Object $Object[$key] -PropertyNames $PropertyNames
      if ($null -ne $value) {
        return $value
      }
    }

    return $null
  }

  if ($Object -is [System.Collections.IEnumerable]) {
    foreach ($item in $Object) {
      $value = Get-JsonPropertyValueRecursive -Object $item -PropertyNames $PropertyNames
      if ($null -ne $value) {
        return $value
      }
    }

    return $null
  }

  foreach ($property in $Object.PSObject.Properties) {
    if ($PropertyNames -contains $property.Name) {
      return $property.Value
    }

    $value = Get-JsonPropertyValueRecursive -Object $property.Value -PropertyNames $PropertyNames
    if ($null -ne $value) {
      return $value
    }
  }

  return $null
}

function Resolve-SignToolPath {
  param(
    [string]$ToolchainJsonPath,
    [string]$RepoRoot
  )

  if ($ToolchainJsonPath) {
    $toolchainAbs = Resolve-AbsolutePath -Path $ToolchainJsonPath -BaseDir $RepoRoot
    if (-not (Test-Path -LiteralPath $toolchainAbs)) {
      throw "Toolchain JSON '$ToolchainJsonPath' not found at '$toolchainAbs'."
    }

    $toolchain = Get-Content -LiteralPath $toolchainAbs -Raw | ConvertFrom-Json
    $signtoolFromJson = Get-JsonPropertyValueRecursive -Object $toolchain -PropertyNames @(
      "signtool",
      "signTool",
      "SignTool",
      "signtoolPath",
      "signToolPath",
      "SignToolPath"
    )

    if ($signtoolFromJson) {
      $candidate = [string]$signtoolFromJson
      if (-not [System.IO.Path]::IsPathRooted($candidate)) {
        $candidate = Join-Path (Split-Path -Parent $toolchainAbs) $candidate
      }

      $candidate = [System.IO.Path]::GetFullPath($candidate)
      if (Test-Path -LiteralPath $candidate) {
        return $candidate
      }

      throw "signtool.exe path from Toolchain JSON does not exist: '$candidate'."
    }
  }

  $signtoolCmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
  if ($signtoolCmd) {
    return $signtoolCmd.Source
  }

  $candidateBases = @()
  if ($env:ProgramFiles -and (Test-Path -LiteralPath $env:ProgramFiles)) {
    $candidateBases += Join-Path $env:ProgramFiles "Windows Kits\\10\\bin"
    $candidateBases += Join-Path $env:ProgramFiles "Windows Kits\\8.1\\bin"
  }
  if ($env:"ProgramFiles(x86)" -and (Test-Path -LiteralPath $env:"ProgramFiles(x86)")) {
    $candidateBases += Join-Path $env:"ProgramFiles(x86)" "Windows Kits\\10\\bin"
    $candidateBases += Join-Path $env:"ProgramFiles(x86)" "Windows Kits\\8.1\\bin"
  }

  $archOrder = @("x64", "x86", "arm64")

  foreach ($base in ($candidateBases | Select-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $base)) {
      continue
    }

    foreach ($arch in $archOrder) {
      $direct = Join-Path $base (Join-Path $arch "signtool.exe")
      if (Test-Path -LiteralPath $direct) {
        return $direct
      }
    }

    $versionDirs = Get-ChildItem -LiteralPath $base -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name -Descending
    foreach ($dir in $versionDirs) {
      foreach ($arch in $archOrder) {
        $candidate = Join-Path $dir.FullName (Join-Path $arch "signtool.exe")
        if (Test-Path -LiteralPath $candidate) {
          return $candidate
        }
      }
    }
  }

  throw "signtool.exe not found. Ensure the Windows SDK is installed or provide -ToolchainJson with a signtool path."
}

function Invoke-SignTool {
  param(
    [Parameter(Mandatory = $true)]
    [string]$SignToolPath,

    [Parameter(Mandatory = $true)]
    [string[]]$Arguments,

    [Parameter(Mandatory = $true)]
    [string]$ContextFile
  )

  $output = & $SignToolPath @Arguments 2>&1
  $exitCode = $LASTEXITCODE

  if ($exitCode -ne 0) {
    $joined = ($output | Out-String).TrimEnd()
    throw "signtool failed (exit $exitCode) for '$ContextFile'.`nCommand: $SignToolPath $($Arguments -join ' ')`n$joined"
  }
}

function Ensure-TrustedCertificate {
  param(
    [Parameter(Mandatory = $true)]
    [string]$CerPath
  )

  $stores = @(
    "Cert:\CurrentUser\Root",
    "Cert:\CurrentUser\TrustedPublisher",
    "Cert:\LocalMachine\Root",
    "Cert:\LocalMachine\TrustedPublisher"
  )

  foreach ($store in $stores) {
    try {
      Import-Certificate -FilePath $CerPath -CertStoreLocation $store -ErrorAction Stop | Out-Null
    } catch {
      Write-Warning "Failed to import '$CerPath' into '$store': $($_.Exception.Message)"
    }
  }
}

function Ensure-TestSigningCertificate {
  param(
    [Parameter(Mandatory = $true)]
    [string]$CerPath,

    [Parameter(Mandatory = $true)]
    [string]$PfxPath,

    [Parameter(Mandatory = $true)]
    [SecureString]$PfxPassword,

    [Parameter(Mandatory = $true)]
    [ValidateSet("sha1", "sha256")]
    [string]$HashAlgorithm
  )

  if (-not (Get-Command New-SelfSignedCertificate -ErrorAction SilentlyContinue)) {
    throw "New-SelfSignedCertificate is not available. Install the PKI module/Windows SDK."
  }

  $shouldGenerate = -not (Test-Path -LiteralPath $CerPath) -or -not (Test-Path -LiteralPath $PfxPath)
  if (-not $shouldGenerate) {
    try {
      $existingCert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($CerPath)
      $existingSigAlg = $existingCert.SignatureAlgorithm.FriendlyName.ToLowerInvariant()
      $existingIsDesiredHash = $existingSigAlg.Contains($HashAlgorithm.ToLowerInvariant())
      $validLongEnough = $existingCert.NotAfter -gt (Get-Date).AddYears(5)
      if (-not $existingIsDesiredHash -or -not $validLongEnough) {
        $shouldGenerate = $true
      }
    } catch {
      $shouldGenerate = $true
    }
  }

  if ($shouldGenerate) {
    $requestedHashAlgorithm = $HashAlgorithm.ToLowerInvariant()
    $notAfter = (Get-Date).AddYears(10)
    $subject = "CN=Aero Test Driver Signing"
    $certHashAlgorithm = $requestedHashAlgorithm

    try {
      $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $subject `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -NotAfter $notAfter `
        -KeyExportPolicy Exportable `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeySpec Signature `
        -HashAlgorithm $certHashAlgorithm `
        -TextExtension @(
          "2.5.29.37={text}1.3.6.1.5.5.7.3.3,1.3.6.1.4.1.311.10.3.6"
        )
    } catch {
      if ($requestedHashAlgorithm -ne "sha1") {
        throw
      }

      Write-Warning "Failed to create a SHA-1-signed self-signed certificate. Falling back to SHA-256: $($_.Exception.Message)"
      $certHashAlgorithm = "sha256"
      $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $subject `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -NotAfter $notAfter `
        -KeyExportPolicy Exportable `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeySpec Signature `
        -HashAlgorithm $certHashAlgorithm `
        -TextExtension @(
          "2.5.29.37={text}1.3.6.1.5.5.7.3.3,1.3.6.1.4.1.311.10.3.6"
        )
    }

    if (-not $cert) {
      throw "New-SelfSignedCertificate did not return a certificate object."
    }

    Export-Certificate -Cert $cert -FilePath $CerPath -Force | Out-Null
    Export-PfxCertificate -Cert $cert -FilePath $PfxPath -Password $PfxPassword -Force | Out-Null
  }

  if (-not (Test-Path -LiteralPath $CerPath)) {
    throw "Expected certificate '$CerPath' to exist after generation."
  }
  if (-not (Test-Path -LiteralPath $PfxPath)) {
    throw "Expected PFX '$PfxPath' to exist after generation."
  }

  Ensure-TrustedCertificate -CerPath $CerPath
}

try {
  $repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))

  $inputRootAbs = Resolve-AbsolutePath -Path $InputRoot -BaseDir $repoRoot
  $certOutDirAbs = Resolve-AbsolutePath -Path $CertOutDir -BaseDir $repoRoot
  $outDirAbs = Resolve-AbsolutePath -Path "out" -BaseDir $repoRoot

  if (-not (Test-Path -LiteralPath $inputRootAbs)) {
    throw "InputRoot '$InputRoot' does not exist at '$inputRootAbs'."
  }

  New-Item -ItemType Directory -Force -Path $certOutDirAbs | Out-Null
  New-Item -ItemType Directory -Force -Path $outDirAbs | Out-Null

  $cerPath = Join-Path $certOutDirAbs "aero-test.cer"
  $pfxPath = Join-Path $outDirAbs "aero-test.pfx"
  $pfxPasswordPlain = "aero-test"
  $pfxPassword = ConvertTo-SecureString -String $pfxPasswordPlain -AsPlainText -Force

  $needsSha1 = $DualSign -or ($Digest.ToLowerInvariant() -eq "sha1")
  $certHashAlgorithm = if ($needsSha1) { "sha1" } else { "sha256" }

  Ensure-TestSigningCertificate -CerPath $cerPath -PfxPath $pfxPath -PfxPassword $pfxPassword -HashAlgorithm $certHashAlgorithm

  $signtoolPath = Resolve-SignToolPath -ToolchainJsonPath $ToolchainJson -RepoRoot $repoRoot
  Write-Host "Using signtool: $signtoolPath"

  $files = @(Get-ChildItem -LiteralPath $inputRootAbs -Recurse -File | Where-Object {
      $ext = $_.Extension.ToLowerInvariant()
      $ext -eq ".sys" -or $ext -eq ".cat"
    })

  if (-not $files -or $files.Count -eq 0) {
    throw "No .sys or .cat files found under '$inputRootAbs'."
  }

  $sysFiles = @($files | Where-Object { $_.Extension.ToLowerInvariant() -eq ".sys" })
  $catFiles = @($files | Where-Object { $_.Extension.ToLowerInvariant() -eq ".cat" })

  $primaryDigest = $Digest.ToLowerInvariant()
  $appendDigest = $null
  if ($DualSign) {
    $primaryDigest = "sha1"
    $appendDigest = "sha256"
  }

  function Sign-File {
    param([string]$Path)

    Invoke-SignTool -SignToolPath $signtoolPath -ContextFile $Path -Arguments @(
      "sign",
      "/v",
      "/f", $pfxPath,
      "/p", $pfxPasswordPlain,
      "/fd", $primaryDigest,
      $Path
    )

    if ($appendDigest) {
      Invoke-SignTool -SignToolPath $signtoolPath -ContextFile $Path -Arguments @(
        "sign",
        "/v",
        "/as",
        "/f", $pfxPath,
        "/p", $pfxPasswordPlain,
        "/fd", $appendDigest,
        $Path
      )
    }
  }

  function Verify-File {
    param(
      [string]$Path,
      [string]$ExtensionLower
    )

    if ($ExtensionLower -eq ".sys") {
      Invoke-SignTool -SignToolPath $signtoolPath -ContextFile $Path -Arguments @(
        "verify",
        "/kp",
        "/v",
        $Path
      )
      return
    }

    if ($ExtensionLower -eq ".cat") {
      Invoke-SignTool -SignToolPath $signtoolPath -ContextFile $Path -Arguments @(
        "verify",
        "/v",
        $Path
      )
      return
    }

    throw "Unexpected file extension for verification: '$Path'"
  }

  foreach ($file in $sysFiles) {
    Write-Host "Signing: $($file.FullName)"
    Sign-File -Path $file.FullName
    Write-Host "Verifying (kernel policy): $($file.FullName)"
    Verify-File -Path $file.FullName -ExtensionLower ".sys"
  }

  foreach ($file in $catFiles) {
    Write-Host "Signing: $($file.FullName)"
    Sign-File -Path $file.FullName
    Write-Host "Verifying (catalog): $($file.FullName)"
    Verify-File -Path $file.FullName -ExtensionLower ".cat"
  }

  if (-not (Test-Path -LiteralPath $cerPath)) {
    throw "Expected '$cerPath' to exist after signing."
  }

  Write-Host "All driver binaries and catalogs were signed and verified successfully."
  exit 0
} catch {
  Write-Error $_
  exit 1
}

