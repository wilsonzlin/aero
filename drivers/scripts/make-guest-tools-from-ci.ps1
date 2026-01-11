#Requires -Version 5.1
#
# Convenience wrapper around `ci/package-guest-tools.ps1` for producing the Guest Tools
# ISO/zip from CI-built signed driver packages (`out/packages` + `out/certs`).
#
# This lives under `drivers/scripts/` so it sits alongside the other "make Guest Tools"
# helper scripts (virtio-win, aero-virtio, etc).
#
[CmdletBinding()]
param(
  # Root directory containing the signed driver packages (default: CI output).
  [string] $PackagesRoot = "out/packages",

  # Directory containing Guest Tools scripts/config/certs (default: repo `guest-tools/`).
  [string] $GuestToolsDir = "guest-tools",

  # Driver signing / boot policy embedded in Guest Tools manifest.json.
  #
  # - test: media is intended for test-signed/custom-signed drivers (default)
  # - production: media is intended for WHQL/production-signed drivers (no cert injection)
  # - none: same as production (development use)
  #
  # Legacy aliases accepted:
  # - testsigning / test-signing -> test
  # - nointegritychecks / no-integrity-checks -> none
  # - prod / whql -> production
  [ValidateSet("test", "production", "none", "testsigning", "test-signing", "prod", "whql", "nointegritychecks", "no-integrity-checks")]
  [string] $SigningPolicy = "test",

  # Public certificate used to sign the driver catalogs (required when SigningPolicy resolves to test).
  [string] $CertPath = "out/certs/aero-test.cer",

  # Packaging spec describing required/optional drivers.
  [string] $SpecPath = "tools/packaging/specs/win7-aero-guest-tools.json",

  # Output directory for `aero-guest-tools.iso`, `aero-guest-tools.zip`, and `manifest.json`.
  [string] $OutDir = "out/artifacts",

  [string] $Version,
  [string] $BuildId,
  [Nullable[long]] $SourceDateEpoch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  return (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}

$repoRoot = Resolve-RepoRoot
$ciScript = Join-Path $repoRoot "ci/package-guest-tools.ps1"
if (-not (Test-Path -LiteralPath $ciScript -PathType Leaf)) {
  throw "Expected CI Guest Tools packager wrapper to exist: $ciScript"
}

& $ciScript `
  -InputRoot $PackagesRoot `
  -GuestToolsDir $GuestToolsDir `
  -SigningPolicy $SigningPolicy `
  -CertPath $CertPath `
  -SpecPath $SpecPath `
  -OutDir $OutDir `
  -Version $Version `
  -BuildId $BuildId `
  -SourceDateEpoch $SourceDateEpoch

if ($LASTEXITCODE -ne 0) {
  throw "ci/package-guest-tools.ps1 failed (exit code $LASTEXITCODE)."
}

