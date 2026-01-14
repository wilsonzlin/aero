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

  # When perf is supported, ensure newer nested objects exist (schema stability).
  if ($ExpectedCommand -eq "status" -and $obj.ok -and $obj.perf -and $obj.perf.supported) {
    if (-not $obj.perf.get_scanline) {
      throw "Missing perf.get_scanline section in status JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
    }
    if (-not $obj.perf.contig_pool) {
      throw "Missing perf.contig_pool section in status JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
    }
  }

  if ($ExpectedCommand -eq "query-perf" -and $obj.ok) {
    if (-not $obj.get_scanline) {
      throw "Missing get_scanline section in query-perf JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
    }
    if (-not $obj.contig_pool) {
      throw "Missing contig_pool section in query-perf JSON for: $DbgctlPath $($Args -join ' ') --json`n$stdout"
    }
  }
}

function Assert-ValidJsonFile {
  param(
    [string]$ExpectedCommand,
    [string[]]$Args,
    [string]$JsonPath,
    [switch]$SeparateArg
  )

  if ([string]::IsNullOrWhiteSpace($JsonPath)) {
    throw "Assert-ValidJsonFile requires -JsonPath"
  }

  Remove-Item -ErrorAction SilentlyContinue $JsonPath

  if ($SeparateArg) {
    $stdout = & $DbgctlPath @Args --json $JsonPath 2>$null
  } else {
    $stdout = & $DbgctlPath @Args "--json=$JsonPath" 2>$null
  }

  $stdoutText = @($stdout) -join "`n"
  if (-not [string]::IsNullOrWhiteSpace($stdoutText)) {
    throw "Expected no stdout when writing JSON to file for: $DbgctlPath $($Args -join ' ')`n$stdoutText"
  }

  if (-not (Test-Path $JsonPath)) {
    throw "JSON output file not created: $JsonPath"
  }

  $jsonText = [System.IO.File]::ReadAllText($JsonPath, [System.Text.Encoding]::UTF8)
  if ([string]::IsNullOrWhiteSpace($jsonText)) {
    throw "JSON output file is empty: $JsonPath"
  }

  try {
    $obj = $jsonText | ConvertFrom-Json
  } catch {
    throw "Invalid JSON in file: $JsonPath`n$jsonText"
  }

  if (-not $obj.schema_version) {
    throw "Missing schema_version in JSON file: $JsonPath`n$jsonText"
  }

  if ($ExpectedCommand -and $obj.command -ne $ExpectedCommand) {
    throw "Unexpected command in JSON file: $JsonPath`nExpected: $ExpectedCommand`nActual: $($obj.command)`n$jsonText"
  }

  if ($ExpectedCommand -eq "status" -and -not $obj.perf) {
    throw "Missing perf section in status JSON file: $JsonPath`n$jsonText"
  }

  if ($ExpectedCommand -eq "status" -and $obj.ok -and $obj.perf -and $obj.perf.supported) {
    if (-not $obj.perf.get_scanline) {
      throw "Missing perf.get_scanline section in status JSON file: $JsonPath`n$jsonText"
    }
    if (-not $obj.perf.contig_pool) {
      throw "Missing perf.contig_pool section in status JSON file: $JsonPath`n$jsonText"
    }
  }

  Remove-Item -ErrorAction SilentlyContinue $JsonPath
}

Assert-ValidJson -ExpectedCommand "status" -Args @("--status")
Assert-ValidJson -ExpectedCommand "status" -Args @("--status", "--pretty")
Assert-ValidJson -ExpectedCommand "help" -Args @("--help")
Assert-ValidJson -ExpectedCommand "help" -Args @("-h")
Assert-ValidJson -ExpectedCommand "help" -Args @("/?")

# File output mode: `--json=PATH` (and `--json PATH`) should create a JSON file and not print to stdout.
Assert-ValidJsonFile -ExpectedCommand "status" -Args @("--status") -JsonPath "status_file_test.json"
Assert-ValidJsonFile -ExpectedCommand "status" -Args @("--status", "--pretty") -JsonPath "status_pretty_file_test.json" -SeparateArg
# Parse errors should still write machine-readable JSON to file if `--json=PATH` is present anywhere.
Assert-ValidJsonFile -ExpectedCommand "parse-args" -Args @("--status", "--query-fence") -JsonPath "parse_args_file_test.json"
Assert-ValidJson -ExpectedCommand "list-displays" -Args @("--list-displays")
Assert-ValidJson -ExpectedCommand "query-fence" -Args @("--query-fence")
Assert-ValidJson -ExpectedCommand "query-error" -Args @("--query-error")
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
Assert-ValidJson -ExpectedCommand "dump-last-cmd" -Args @("--dump-last-submit", "--out", "last_cmd_submit_out_test.bin")
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
  # If the ring only has one descriptor available, --count 2 may fall back to a single output at the base path.
  "last_cmd_test_multi.bin",
  "last_cmd_test_multi.bin.txt",
  "last_cmd_test_multi.bin.alloc_table.bin",
  "last_cmd_submit_test.bin",
  "last_cmd_submit_test.bin.txt",
  "last_cmd_submit_test.bin.alloc_table.bin",
  "last_cmd_submit_out_test.bin",
  "last_cmd_submit_out_test.bin.txt",
  "last_cmd_submit_out_test.bin.alloc_table.bin",
  "last_cmd_out_test.bin",
  "last_cmd_out_test.bin.txt",
  "last_cmd_out_test.bin.alloc_table.bin",
  "read_gpa_test.bin",
  "read_gpa_chunked_test.bin",
  "status_file_test.json",
  "status_pretty_file_test.json",
  "parse_args_file_test.json",
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

