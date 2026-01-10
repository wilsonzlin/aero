Param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [String[]] $Args
)

$DockerBin = if ($env:DOCKER_BIN) { $env:DOCKER_BIN } else { "docker" }
$Image = if ($env:AERO_WIN7_SLIPSTREAM_IMAGE) { $env:AERO_WIN7_SLIPSTREAM_IMAGE } else { "aero/win7-slipstream" }

if (-not (Get-Command $DockerBin -ErrorAction SilentlyContinue)) {
  Write-Error "'$DockerBin' not found in PATH (install Docker Desktop, or set DOCKER_BIN=podman)."
  exit 127
}

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\\..\\..")
$Dockerfile = Join-Path $RepoRoot "tools\\win7-slipstream\\container\\Dockerfile"

& $DockerBin image inspect $Image *> $null
if ($LASTEXITCODE -ne 0) {
  Write-Host "info: image '$Image' not found; building it..."
  & $DockerBin build -t $Image -f $Dockerfile $RepoRoot
  if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
  }
}

$PwdPath = (Get-Location).Path

& $DockerBin run --rm -it `
  --mount "type=bind,source=$PwdPath,target=/work" `
  -w /work `
  $Image `
  @Args

exit $LASTEXITCODE

