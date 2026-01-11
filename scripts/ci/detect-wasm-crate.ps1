#!/usr/bin/env pwsh
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$resolver = Join-Path $scriptDir "detect-wasm-crate.mjs"

if (-not (Test-Path -LiteralPath $resolver)) {
  Write-Error "detect-wasm-crate.mjs not found next to this script: $resolver"
  exit 1
}

try {
  & node $resolver @args
  exit $LASTEXITCODE
} catch {
  Write-Error "Failed to run Node.js. Ensure Node is installed and available on PATH."
  throw
}

