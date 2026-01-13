param(
  [string]$DbgctlPath = ".\\aerogpu_dbgctl.exe"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $DbgctlPath)) {
  throw "aerogpu_dbgctl.exe not found: $DbgctlPath"
}

function Assert-ValidJson {
  param(
    [string]$ExpectedCommand,
    [string[]]$Args
  )

  $stdout = & $DbgctlPath @Args --json 2>$null
  if ([string]::IsNullOrWhiteSpace($stdout)) {
    throw "No JSON output for: $DbgctlPath $($Args -join ' ') --json"
  }

  try {
    $obj = $stdout | ConvertFrom-Json
  } catch {
    throw "Invalid JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
  }

  if (-not $obj.schema_version) {
    throw "Missing schema_version for: $DbgctlPath $($Args -join ' ') --json"
  }

  if ($ExpectedCommand -and $obj.command -ne $ExpectedCommand) {
    throw "Unexpected command in JSON for: $DbgctlPath $($Args -join ' ') --json`nExpected: $ExpectedCommand`nActual: $($obj.command)`n$stdout"
  }
}

Assert-ValidJson -ExpectedCommand "status" -Args @("--status")
Assert-ValidJson -ExpectedCommand "status" -Args @("--status", "--pretty")
Assert-ValidJson -ExpectedCommand "query-fence" -Args @("--query-fence")
Assert-ValidJson -ExpectedCommand "query-perf" -Args @("--query-perf")
Assert-ValidJson -ExpectedCommand "query-scanout" -Args @("--query-scanout")
Assert-ValidJson -ExpectedCommand "query-cursor" -Args @("--query-cursor")
Assert-ValidJson -ExpectedCommand "dump-ring" -Args @("--dump-ring", "--ring-id", "0")
Assert-ValidJson -ExpectedCommand "dump-vblank" -Args @("--dump-vblank", "--vblank-samples", "1")

Write-Host "OK: dbgctl JSON output parsed successfully"

