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

  # Basic schema sanity for status payloads: ensure the perf section is present
  # (it may be supported:false on older KMDs, but should still exist).
  if ($ExpectedCommand -eq "status" -and -not $obj.perf) {
    throw "Missing perf section in status JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
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
Assert-ValidJson -ExpectedCommand "query-scanline" -Args @("--query-scanline", "--vblank-samples", "1", "--vblank-interval-ms", "0")
Assert-ValidJson -ExpectedCommand "wait-vblank" -Args @("--wait-vblank", "--vblank-samples", "2", "--timeout-ms", "200")
Assert-ValidJson -ExpectedCommand "dump-scanout-bmp" -Args @("--dump-scanout-bmp", "scanout_test.bmp")
Assert-ValidJson -ExpectedCommand "dump-scanout-png" -Args @("--dump-scanout-png", "scanout_test.png")
Assert-ValidJson -ExpectedCommand "dump-cursor-bmp" -Args @("--dump-cursor-bmp", "cursor_test.bmp")
Assert-ValidJson -ExpectedCommand "dump-cursor-png" -Args @("--dump-cursor-png", "cursor_test.png")

Write-Host "OK: dbgctl JSON output parsed successfully"

