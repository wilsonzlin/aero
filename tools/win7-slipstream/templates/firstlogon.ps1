param(
  [string]$AeroRoot = "$env:SystemDrive\Aero",
  [string]$CertFileName = "{{AERO_CERT_FILENAME}}",
  [string]$DriverDir = "$env:SystemDrive\Aero\drivers",
  [string]$SigningMode = "{{AERO_SIGNING_MODE}}"
)

$ErrorActionPreference = "Stop"

function Add-CertToLocalMachineStore {
  param(
    [Parameter(Mandatory = $true)][string]$StoreName,
    [Parameter(Mandatory = $true)][string]$CertPath
  )

  if (-not (Test-Path -LiteralPath $CertPath)) {
    Write-Warning "Cert not found: $CertPath"
    return
  }

  $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($CertPath)
  $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($StoreName, "LocalMachine")
  $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
  try {
    $store.Add($cert)
  } finally {
    $store.Close()
  }
}

$certPath = Join-Path (Join-Path $AeroRoot "certs") $CertFileName

Write-Host "[Aero] FirstLogon starting..."
Write-Host ("[Aero] AeroRoot      = {0}" -f $AeroRoot)
Write-Host ("[Aero] CertPath      = {0}" -f $certPath)
Write-Host ("[Aero] DriverDir     = {0}" -f $DriverDir)
Write-Host ("[Aero] SigningMode   = {0}" -f $SigningMode)

Write-Host "[Aero] Installing test root cert..."
Add-CertToLocalMachineStore -StoreName "Root" -CertPath $certPath
Add-CertToLocalMachineStore -StoreName "TrustedPublisher" -CertPath $certPath

Write-Host "[Aero] Setting boot signing policy..."
if ($SigningMode -ieq "testsigning") {
  & bcdedit /set "{current}" testsigning on | Out-Null
  & bcdedit /set "{current}" nointegritychecks off | Out-Null
} elseif ($SigningMode -ieq "nointegritychecks") {
  & bcdedit /set "{current}" testsigning off | Out-Null
  & bcdedit /set "{current}" nointegritychecks on | Out-Null
} else {
  Write-Warning ("Unknown SigningMode '{0}'. Expected 'testsigning' or 'nointegritychecks'." -f $SigningMode)
}

if (Test-Path -LiteralPath $DriverDir) {
  Write-Host "[Aero] Installing drivers via pnputil..."
  Get-ChildItem -LiteralPath $DriverDir -Recurse -Filter *.inf | ForEach-Object {
    Write-Host ("[Aero] pnputil -i -a {0}" -f $_.FullName)
    & pnputil -i -a $_.FullName | Out-Null
  }
} else {
  Write-Warning "DriverDir not found: $DriverDir"
}

Write-Host "[Aero] FirstLogon finished."
