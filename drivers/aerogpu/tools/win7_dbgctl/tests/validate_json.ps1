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
Assert-ValidJson -ExpectedCommand "help" -Args @("--help")
Assert-ValidJson -ExpectedCommand "list-displays" -Args @("--list-displays")
Assert-ValidJson -ExpectedCommand "query-fence" -Args @("--query-fence")
Assert-ValidJson -ExpectedCommand "watch-fence" -Args @("--watch-fence", "--samples", "2", "--interval-ms", "0")
Assert-ValidJson -ExpectedCommand "query-perf" -Args @("--query-perf")
Assert-ValidJson -ExpectedCommand "query-scanout" -Args @("--query-scanout")
Assert-ValidJson -ExpectedCommand "query-cursor" -Args @("--query-cursor")
Assert-ValidJson -ExpectedCommand "query-umd-private" -Args @("--query-umd-private")
Assert-ValidJson -ExpectedCommand "query-segments" -Args @("--query-segments")
Assert-ValidJson -ExpectedCommand "dump-ring" -Args @("--dump-ring", "--ring-id", "0")
Assert-ValidJson -ExpectedCommand "watch-ring" -Args @("--watch-ring", "--ring-id", "0", "--samples", "1", "--interval-ms", "1")
Assert-ValidJson -ExpectedCommand "dump-last-cmd" -Args @("--dump-last-cmd", "--cmd-out", "last_cmd_test.bin")
Assert-ValidJson -ExpectedCommand "dump-last-cmd" -Args @("--dump-last-cmd", "--count", "2", "--cmd-out", "last_cmd_test_multi.bin")
Assert-ValidJson -ExpectedCommand "dump-last-cmd" -Args @("--dump-last-submit", "--cmd-out", "last_cmd_submit_test.bin")
Assert-ValidJson -ExpectedCommand "dump-last-cmd" -Args @("--dump-last-cmd", "--out", "last_cmd_out_test.bin")
Assert-ValidJson -ExpectedCommand "dump-createalloc" -Args @("--dump-createalloc")
Assert-ValidJson -ExpectedCommand "dump-vblank" -Args @("--dump-vblank", "--vblank-samples", "1")
Assert-ValidJson -ExpectedCommand "query-scanline" -Args @("--query-scanline", "--vblank-samples", "1", "--vblank-interval-ms", "0")
Assert-ValidJson -ExpectedCommand "wait-vblank" -Args @("--wait-vblank", "--vblank-samples", "2", "--timeout-ms", "200")
Assert-ValidJson -ExpectedCommand "selftest" -Args @("--selftest", "--timeout-ms", "2000")
Assert-ValidJson -ExpectedCommand "read-gpa" -Args @("--read-gpa", "0x0", "--size", "4", "--out", "read_gpa_test.bin")
Assert-ValidJson -ExpectedCommand "dump-scanout-bmp" -Args @("--dump-scanout-bmp", "scanout_test.bmp")
Assert-ValidJson -ExpectedCommand "dump-scanout-png" -Args @("--dump-scanout-png", "scanout_test.png")
Assert-ValidJson -ExpectedCommand "dump-cursor-bmp" -Args @("--dump-cursor-bmp", "cursor_test.bmp")
Assert-ValidJson -ExpectedCommand "dump-cursor-png" -Args @("--dump-cursor-png", "cursor_test.png")

# Parse errors should still return machine-readable JSON if `--json` is present.
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--status", "--query-fence")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--size", "nope", "--read-gpa", "0x0")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--ring-id", "nope", "--dump-ring")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--timeout-ms", "nope", "--status")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--watch-fence", "--samples", "nope", "--interval-ms", "0")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--watch-fence", "--samples", "1", "--interval-ms", "nope")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--dump-vblank", "--vblank-samples", "nope")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--dump-vblank", "--vblank-interval-ms", "nope")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--status", "--out", "out.bin")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--status", "--cmd-out", "cmd.bin")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--status", "--alloc-out", "alloc.bin")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--dump-last-cmd", "--cmd-out", "cmd.bin", "--alloc-out", "a.bin", "--alloc-out", "b.bin")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--read-gpa", "0x0", "4", "--out", "a.bin", "--out", "b.bin")
Assert-ValidJson -ExpectedCommand "read-gpa" -Args @("--read-gpa", "0x0", "nope")
Assert-ValidJson -ExpectedCommand "read-gpa" -Args @("--size", "4", "--read-gpa", "0x0", "4")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--dump-createalloc", "--csv", "a.csv", "--csv", "b.csv")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--json=")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--size", "4", "--size", "8", "--read-gpa", "0x0")
Assert-ValidJson -ExpectedCommand "parse-args" -Args @("--read-gpa", "0x0", "--size", "4", "--out", "a.bin", "--out", "b.bin")
Assert-ValidJson -ExpectedCommand "read-gpa" -Args @("--size", "4", "--read-gpa", "0x0", "8")
Assert-ValidJson -ExpectedCommand "read-gpa" -Args @("--read-gpa", "0x0", "--size", "5000", "--out", "read_gpa_chunked_test.bin")

# Best-effort cleanup to avoid clutter in local runs. On failure, artifacts may be left behind for debugging.
$artifacts = @(
  "last_cmd_test.bin",
  "last_cmd_test.bin.txt",
  "last_cmd_test.bin.alloc_table.bin",
  "last_cmd_test_multi_0.bin",
  "last_cmd_test_multi_0.bin.txt",
  "last_cmd_test_multi_0.bin.alloc_table.bin",
  "last_cmd_test_multi_1.bin",
  "last_cmd_test_multi_1.bin.txt",
  "last_cmd_test_multi_1.bin.alloc_table.bin",
  "last_cmd_submit_test.bin",
  "last_cmd_submit_test.bin.txt",
  "last_cmd_submit_test.bin.alloc_table.bin",
  "last_cmd_out_test.bin",
  "last_cmd_out_test.bin.txt",
  "last_cmd_out_test.bin.alloc_table.bin",
  "read_gpa_test.bin",
  "read_gpa_chunked_test.bin",
  "scanout_test.bmp",
  "scanout_test.png",
  "cursor_test.bmp",
  "cursor_test.png",
  "foo.bin"
)
foreach ($f in $artifacts) {
  Remove-Item -ErrorAction SilentlyContinue $f
}

Write-Host "OK: dbgctl JSON output parsed successfully"

