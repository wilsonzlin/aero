#Requires -Version 5.1
<#
.SYNOPSIS
Create a CI test code signing certificate and sign driver artifacts.

.DESCRIPTION
This script is intended for CI use (e.g. GitHub Actions Windows runners).
It creates a self-signed code signing certificate and signs the provided
driver artifacts using `signtool`.

Windows 7 SHA-1/SHA-2 compatibility note:
When signing files with `/fd sha1`, Windows 7 without SHA-2 updates
(KB3033929 / KB4474419) can still fail if the *certificate itself* is signed
using SHA-256. For maximum compatibility, when `-Digest sha1` (or `-DualSign`)
is selected this script attempts to create the self-signed certificate using
`New-SelfSignedCertificate -HashAlgorithm sha1`.

If the runner cannot create SHA-1-signed certificates, the script fails unless
`-AllowSha2CertFallback` is explicitly provided, in which case it falls back to
creating a SHA-256-signed certificate with a loud warning.

.PARAMETER Path
Files or directories to sign. Directories are searched recursively for common
driver artifacts: *.sys, *.cat, *.dll, *.exe.

.PARAMETER Digest
File digest algorithm passed to `signtool sign /fd` when not using `-DualSign`.
Valid values: sha1, sha256.

.PARAMETER DualSign
If set, performs dual signing: first `/fd sha1`, then appends a second
signature using `/fd sha256` (`signtool /as`).

.PARAMETER AllowSha2CertFallback
If set, and SHA-1 certificate creation fails, the script will fall back to a
SHA-256-signed self-signed certificate and continue. This may produce binaries
that fail to validate on stock Windows 7 SP1 without KB3033929/KB4474419.

.EXAMPLE
.\ci\sign-drivers.ps1 -Path .\out\drivers -Digest sha1

.EXAMPLE
.\ci\sign-drivers.ps1 -Path .\out\drivers -DualSign
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string[]] $Path,

    [Parameter()]
    [ValidateSet('sha1', 'sha256')]
    [string] $Digest = 'sha256',

    [Parameter()]
    [switch] $DualSign,

    [Parameter()]
    [switch] $AllowSha2CertFallback,

    [Parameter()]
    [string] $CertSubject = 'CN=Aero CI Test Code Signing'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Section([string] $Title) {
    Write-Host ""
    Write-Host "== $Title =="
}

function Resolve-SigntoolPath {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Path
    }

    $candidateRoots = @()
    if ($env:ProgramFiles) {
        $candidateRoots += (Join-Path $env:ProgramFiles 'Windows Kits\10\bin')
    }
    if (${env:ProgramFiles(x86)}) {
        $candidateRoots += (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin')
    }

    foreach ($root in $candidateRoots) {
        if (-not (Test-Path -LiteralPath $root)) {
            continue
        }

        # Prefer the highest version and x64 when available.
        $versionDirs = Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue | Sort-Object -Property Name -Descending
        foreach ($verDir in $versionDirs) {
            foreach ($arch in @('x64', 'x86', 'arm64')) {
                $exe = Join-Path (Join-Path $verDir.FullName $arch) 'signtool.exe'
                if (Test-Path -LiteralPath $exe) {
                    return $exe
                }
            }
        }

        foreach ($arch in @('x64', 'x86', 'arm64')) {
            $exe = Join-Path (Join-Path $root $arch) 'signtool.exe'
            if (Test-Path -LiteralPath $exe) {
                return $exe
            }
        }
    }

    throw "signtool.exe not found. Install the Windows SDK 'Signing Tools' feature or add signtool.exe to PATH."
}

function Expand-SignableFiles([string[]] $InputPaths) {
    $allowedExtensions = @('.sys', '.cat', '.dll', '.exe')
    $files = New-Object System.Collections.Generic.List[string]

    foreach ($p in $InputPaths) {
        $resolved = Resolve-Path -Path $p -ErrorAction Stop
        foreach ($rp in $resolved) {
            if (Test-Path -LiteralPath $rp.Path -PathType Container) {
                Get-ChildItem -LiteralPath $rp.Path -Recurse -File |
                    Where-Object { $allowedExtensions -contains $_.Extension.ToLowerInvariant() } |
                    ForEach-Object { $files.Add($_.FullName) }
            }
            else {
                $files.Add($rp.Path)
            }
        }
    }

    return $files | Sort-Object -Unique
}

function New-CodeSigningCertificate([string] $HashAlgorithm) {
    $notAfter = (Get-Date).AddYears(5)

    return New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $CertSubject `
        -CertStoreLocation 'Cert:\CurrentUser\My' `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeyExportPolicy Exportable `
        -KeySpec Signature `
        -HashAlgorithm $HashAlgorithm `
        -NotAfter $notAfter
}

function Write-CertificateInfo($Cert) {
    $sigFriendly = $Cert.SignatureAlgorithm.FriendlyName
    $sigOid = $Cert.SignatureAlgorithm.Value

    Write-Host "Certificate:"
    Write-Host "  Subject:            $($Cert.Subject)"
    Write-Host "  Thumbprint:         $($Cert.Thumbprint)"
    Write-Host "  SignatureAlgorithm: $sigFriendly ($sigOid)"
    Write-Host "  NotAfter:           $($Cert.NotAfter.ToString('u'))"
}

function Invoke-Signtool([string[]] $Args) {
    Write-Host "signtool $($Args -join ' ')"
    & $script:SigntoolPath @Args
    if ($LASTEXITCODE -ne 0) {
        throw "signtool failed with exit code $LASTEXITCODE"
    }
}

Write-Section "Inputs"
Write-Host "Digest:    $Digest"
Write-Host "DualSign:  $DualSign"
Write-Host "Subject:   $CertSubject"
Write-Host "Paths:     $($Path -join ', ')"

# If we are producing a SHA-1 file signature (explicit sha1 digest or dual-sign),
# try to also create a SHA-1-signed certificate for maximum Win7 compatibility.
$desiredCertHash = if (($Digest -eq 'sha1') -or $DualSign) { 'sha1' } else { 'sha256' }

Write-Section "Creating self-signed certificate (requested: $desiredCertHash)"
$cert = $null
try {
    $cert = New-CodeSigningCertificate -HashAlgorithm $desiredCertHash
}
catch {
    if ($desiredCertHash -ne 'sha1') {
        throw
    }

    Write-Warning "Requested a SHA-1-signed certificate (-HashAlgorithm sha1) but certificate creation failed on this runner."
    Write-Warning "Error: $($_.Exception.Message)"

    if (-not $AllowSha2CertFallback) {
        throw "Refusing to proceed without a SHA-1-signed certificate. Re-run with -AllowSha2CertFallback to continue anyway (may break stock Win7 without KB3033929/KB4474419)."
    }

    Write-Warning "Proceeding due to -AllowSha2CertFallback: creating a SHA-256-signed certificate instead."
    Write-Warning "WARNING: Stock Windows 7 SP1 without KB3033929 (kernel-mode SHA-2 support) / KB4474419 (general SHA-2 support) may fail to validate the signature chain, even if /fd sha1 is used."

    $cert = New-CodeSigningCertificate -HashAlgorithm 'sha256'
}

Write-Section "Certificate details"
Write-CertificateInfo -Cert $cert

$script:SigntoolPath = Resolve-SigntoolPath
Write-Host "Using signtool: $script:SigntoolPath"

$files = @(Expand-SignableFiles -InputPaths $Path)
if ($files.Count -eq 0) {
    throw "No signable files found in: $($Path -join ', ')"
}

Write-Section "Signing files"
foreach ($file in $files) {
    Write-Host " - $file"
}

foreach ($file in $files) {
    Write-Section "Signing: $file"
    if ($DualSign) {
        # Sign SHA-1 first, then append SHA-256 (dual signing).
        Invoke-Signtool -Args @('sign', '/v', '/fd', 'sha1', '/sha1', $cert.Thumbprint, '/s', 'My', $file)
        Invoke-Signtool -Args @('sign', '/v', '/as', '/fd', 'sha256', '/sha1', $cert.Thumbprint, '/s', 'My', $file)
    }
    else {
        Invoke-Signtool -Args @('sign', '/v', '/fd', $Digest, '/sha1', $cert.Thumbprint, '/s', 'My', $file)
    }

    Write-Section "Verifying: $file"
    Invoke-Signtool -Args @('verify', '/pa', '/v', '/all', $file)
}

Write-Host ""
Write-Host "Done."

