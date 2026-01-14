Set-StrictMode -Version Latest

function Get-PathStringComparison {
  [CmdletBinding()]
  param()

  # Prefer case-insensitive comparisons on Windows (drive letters, NTFS), but preserve
  # case-sensitive semantics elsewhere.
  try {
    if ($PSVersionTable.PSVersion.Major -ge 6) {
      if ($IsWindows) {
        return [System.StringComparison]::OrdinalIgnoreCase
      }
    }
  } catch {
    # ignore and fall back to platform detection below
  }

  try {
    if ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT) {
      return [System.StringComparison]::OrdinalIgnoreCase
    }
  } catch {
    # ignore and fall back to ordinal
  }

  return [System.StringComparison]::Ordinal
}

function Assert-PathIsRelativeAndUnderRoot {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string] $Root,

    [Parameter(Mandatory = $true)]
    [string] $ChildPath,

    [Parameter(Mandatory = $true)]
    [string] $Context,

    [Parameter(Mandatory = $true)]
    [string] $ManifestPath
  )

  if ([System.IO.Path]::IsPathRooted($ChildPath)) {
    throw "Invalid manifest '$ManifestPath': $Context must be a relative path (got rooted path '$ChildPath')."
  }

  Assert-PathDoesNotContainDotDot -Path $ChildPath -Context $Context -ManifestPath $ManifestPath

  $sep = [System.IO.Path]::DirectorySeparatorChar
  $alt = [System.IO.Path]::AltDirectorySeparatorChar

  $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd($sep, $alt)
  $full = [System.IO.Path]::GetFullPath((Join-Path -Path $rootFull -ChildPath $ChildPath))

  $prefix = $rootFull + $sep
  $cmp = Get-PathStringComparison

  if (-not $full.StartsWith($prefix, $cmp)) {
    throw "Invalid manifest '$ManifestPath': $Context path '$ChildPath' resolves outside driver root '$rootFull'."
  }
}

function Assert-PathDoesNotContainDotDot {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string] $Path,

    [Parameter(Mandatory = $true)]
    [string] $Context,

    [Parameter(Mandatory = $true)]
    [string] $ManifestPath
  )

  # Disallow '..' traversal segments even if the normalized path would still be under the driver root.
  # This prevents surprising/manipulative manifests like "foo/../bar.txt".
  if ($Path -match '(^|[\\\\/])\\.\\.([\\\\/]|$)') {
    throw "Invalid manifest '$ManifestPath': $Context must not contain '..' path segments (got '$Path')."
  }
}

function Assert-JsonString {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    $Value,

    [Parameter(Mandatory = $true)]
    [string] $Context,

    [Parameter(Mandatory = $true)]
    [string] $ManifestPath
  )

  if ($null -eq $Value) {
    throw "Invalid manifest '$ManifestPath': $Context must be a string; got null."
  }

  if (-not ($Value -is [string])) {
    throw "Invalid manifest '$ManifestPath': $Context must be a string; got type '$($Value.GetType().FullName)'."
  }

  $trimmed = $Value.Trim()
  if ([string]::IsNullOrWhiteSpace($trimmed)) {
    throw "Invalid manifest '$ManifestPath': $Context must be a non-empty string."
  }
}

function Assert-JsonArray {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    $Value,

    [Parameter(Mandatory = $true)]
    [string] $Context,

    [Parameter(Mandatory = $true)]
    [string] $ManifestPath
  )

  if ($null -eq $Value) {
    throw "Invalid manifest '$ManifestPath': $Context must be an array; got null."
  }

  if (-not ($Value -is [System.Array])) {
    throw "Invalid manifest '$ManifestPath': $Context must be an array; got type '$($Value.GetType().FullName)'."
  }
}

function Assert-JsonObject {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    $Value,

    [Parameter(Mandatory = $true)]
    [string] $Context,

    [Parameter(Mandatory = $true)]
    [string] $ManifestPath
  )

  if ($null -eq $Value) {
    throw "Invalid manifest '$ManifestPath': $Context must be an object; got null."
  }

  if ($Value -is [System.Array]) {
    throw "Invalid manifest '$ManifestPath': $Context must be an object; got array."
  }

  if ($Value -is [string] -or $Value -is [ValueType]) {
    throw "Invalid manifest '$ManifestPath': $Context must be an object; got type '$($Value.GetType().FullName)'."
  }

  # PSCustomObject exposes properties via PSObject.Properties.
  if ($null -eq $Value.PSObject -or $null -eq $Value.PSObject.Properties) {
    throw "Invalid manifest '$ManifestPath': $Context must be an object."
  }
}

function Validate-DriverPackageManifest {
  <#
    .SYNOPSIS
      Validates a driver packaging manifest (ci-package.json) with strict schema-like checks.

    .DESCRIPTION
      CI scripts use ci-package.json as an opt-in gate for staging/signing driver packages.
      Historically, typos (unknown keys) could silently change packaging behaviour. This
      validator rejects unknown fields and validates the key invariants implied by
      ci/driver-package.schema.json.

    .PARAMETER ManifestPath
      Path to the ci-package.json file.

    .PARAMETER DriverRoot
      Optional driver root directory used to validate that relative paths do not escape the
      driver tree.

    .OUTPUTS
      The parsed JSON object (PSCustomObject) if validation succeeds.
  #>
  [CmdletBinding()]
  param(
    [Parameter(Mandatory = $true)]
    [string] $ManifestPath,

    [string] $DriverRoot
  )

  if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
    throw "Driver package manifest not found: $ManifestPath"
  }

  $raw = Get-Content -LiteralPath $ManifestPath -Raw -ErrorAction Stop
  if ([string]::IsNullOrWhiteSpace($raw)) {
    throw "Driver package manifest '$ManifestPath' is empty."
  }

  $data = $null
  try {
    $convertArgs = @{}
    try {
      $cmd = Get-Command -Name ConvertFrom-Json -ErrorAction SilentlyContinue
      if ($cmd -and $cmd.Parameters -and $cmd.Parameters.ContainsKey('Depth')) {
        $convertArgs['Depth'] = 20
      }
    } catch {
      # ignore; fall back to default depth
    }

    $data = $raw | ConvertFrom-Json @convertArgs -ErrorAction Stop
  } catch {
    throw "Failed to parse driver package manifest JSON '$ManifestPath': $($_.Exception.Message)"
  }

  Assert-JsonObject -Value $data -Context 'manifest root' -ManifestPath $ManifestPath

  $allowedTopLevel = @(
    '$schema',
    'infFiles',
    'wow64Files',
    'requiredBuildOutputFiles',
    'additionalFiles',
    'toolFiles',
    'wdfCoInstaller'
  )
  $unknownKeys = @()
  foreach ($prop in $data.PSObject.Properties) {
    if ($allowedTopLevel -cnotcontains $prop.Name) {
      $unknownKeys += $prop.Name
    }
  }

  if ($unknownKeys.Count -gt 0) {
    $sorted = $unknownKeys | Sort-Object
    $allowed = $allowedTopLevel -join ', '
    throw "Invalid manifest '$ManifestPath': unknown top-level key(s): $($sorted -join ', '). Allowed keys: $allowed"
  }

  $schemaProp = $data.PSObject.Properties['$schema']
  if ($null -ne $schemaProp) {
    Assert-JsonString -Value $schemaProp.Value -Context '$schema' -ManifestPath $ManifestPath
  }

  $infProp = $data.PSObject.Properties['infFiles']
  if ($null -ne $infProp) {
    Assert-JsonArray -Value $infProp.Value -Context 'infFiles' -ManifestPath $ManifestPath
    $entries = $infProp.Value
    if ($entries.Count -lt 1) {
      throw "Invalid manifest '$ManifestPath': infFiles is present but empty."
    }

    $seen = @{}
    $index = 0
    foreach ($entry in $entries) {
      $index++
      Assert-JsonString -Value $entry -Context "infFiles[$index]" -ManifestPath $ManifestPath
      $s = ([string]$entry).Trim()

      if ([System.IO.Path]::GetExtension($s).ToLowerInvariant() -ne '.inf') {
        throw "Invalid manifest '$ManifestPath': infFiles[$index] '$s' must end with '.inf'."
      }

      if ($DriverRoot) {
        Assert-PathIsRelativeAndUnderRoot -Root $DriverRoot -ChildPath $s -Context "infFiles[$index]" -ManifestPath $ManifestPath
      } elseif ([System.IO.Path]::IsPathRooted($s)) {
        throw "Invalid manifest '$ManifestPath': infFiles[$index] must be a relative path (got '$s')."
      } else {
        Assert-PathDoesNotContainDotDot -Path $s -Context "infFiles[$index]" -ManifestPath $ManifestPath
      }

      $key = $s.Replace('\', '/').ToLowerInvariant()
      if ($seen.ContainsKey($key)) {
        throw "Invalid manifest '$ManifestPath': infFiles contains a duplicate entry '$s'."
      }
      $seen[$key] = $true
    }
  }

  $wowProp = $data.PSObject.Properties['wow64Files']
  if ($null -ne $wowProp) {
    Assert-JsonArray -Value $wowProp.Value -Context 'wow64Files' -ManifestPath $ManifestPath

    $seen = @{}
    $index = 0
    foreach ($entry in $wowProp.Value) {
      $index++
      Assert-JsonString -Value $entry -Context "wow64Files[$index]" -ManifestPath $ManifestPath
      $s = ([string]$entry).Trim()

      if ([System.IO.Path]::IsPathRooted($s)) {
        throw "Invalid manifest '$ManifestPath': wow64Files[$index] '$s' must be a file name, not a path."
      }

      if ($s.IndexOfAny(@([char]'\', [char]'/')) -ge 0) {
        throw "Invalid manifest '$ManifestPath': wow64Files[$index] '$s' must be a file name, not a path."
      }

      if ([System.IO.Path]::GetExtension($s).ToLowerInvariant() -ne '.dll') {
        throw "Invalid manifest '$ManifestPath': wow64Files[$index] '$s' must end with '.dll'."
      }

      $key = $s.ToLowerInvariant()
      if ($seen.ContainsKey($key)) {
        throw "Invalid manifest '$ManifestPath': wow64Files contains a duplicate entry '$s'."
      }
      $seen[$key] = $true
    }
  }

  $additionalProp = $data.PSObject.Properties['additionalFiles']
  if ($null -ne $additionalProp) {
    Assert-JsonArray -Value $additionalProp.Value -Context 'additionalFiles' -ManifestPath $ManifestPath

    $binaryExts = @(
      '.sys', '.dll', '.exe', '.cat', '.msi', '.cab',
      '.pdb', '.dbg', '.ipdb', '.iobj', '.obj', '.lib', '.exp', '.ilk', '.idb', '.map', '.tlog',
      '.pch', '.sdf', '.opensdf', '.ncb', '.binlog', '.etl', '.dmp', '.tmp', '.cache'
    )
    $secretExts = @(
      '.pfx',
      '.p12',
      '.pvk',
      '.snk',
      '.key',
      '.pem',
      '.p8',
      '.ppk',
      '.jks',
      '.keystore',
      '.kdbx',
      '.gpg',
      '.pgp'
    )

    $seen = @{}
    $index = 0
    foreach ($entry in $additionalProp.Value) {
      $index++
      Assert-JsonString -Value $entry -Context "additionalFiles[$index]" -ManifestPath $ManifestPath
      $s = ([string]$entry).Trim()

      if ($DriverRoot) {
        Assert-PathIsRelativeAndUnderRoot -Root $DriverRoot -ChildPath $s -Context "additionalFiles[$index]" -ManifestPath $ManifestPath
      } elseif ([System.IO.Path]::IsPathRooted($s)) {
        throw "Invalid manifest '$ManifestPath': additionalFiles[$index] must be a relative path (got '$s')."
      } else {
        Assert-PathDoesNotContainDotDot -Path $s -Context "additionalFiles[$index]" -ManifestPath $ManifestPath
      }

      $ext = [System.IO.Path]::GetExtension($s).ToLowerInvariant()
      if ($binaryExts -contains $ext) {
        if ($ext -eq '.exe') {
          throw "Invalid manifest '$ManifestPath': additionalFiles[$index] '$s' must not include binary extension '$ext'. Use toolFiles to include helper .exe binaries."
        }
        throw "Invalid manifest '$ManifestPath': additionalFiles[$index] '$s' must not include binary extension '$ext'."
      }
      if ($secretExts -contains $ext) {
        throw "Invalid manifest '$ManifestPath': additionalFiles[$index] '$s' must not include sensitive/secret extension '$ext'."
      }

      $key = $s.Replace('\', '/').ToLowerInvariant()
      if ($seen.ContainsKey($key)) {
        throw "Invalid manifest '$ManifestPath': additionalFiles contains a duplicate entry '$s'."
      }
      $seen[$key] = $true
    }
  }

  $requiredBuildOutputProp = $data.PSObject.Properties['requiredBuildOutputFiles']
  if ($null -ne $requiredBuildOutputProp) {
    Assert-JsonArray -Value $requiredBuildOutputProp.Value -Context 'requiredBuildOutputFiles' -ManifestPath $ManifestPath

    $seen = @{}
    $index = 0
    foreach ($entry in $requiredBuildOutputProp.Value) {
      $index++
      Assert-JsonString -Value $entry -Context "requiredBuildOutputFiles[$index]" -ManifestPath $ManifestPath
      $s = ([string]$entry).Trim()

      if ($DriverRoot) {
        Assert-PathIsRelativeAndUnderRoot -Root $DriverRoot -ChildPath $s -Context "requiredBuildOutputFiles[$index]" -ManifestPath $ManifestPath
      } elseif ([System.IO.Path]::IsPathRooted($s)) {
        throw "Invalid manifest '$ManifestPath': requiredBuildOutputFiles[$index] must be a relative path (got '$s')."
      } else {
        Assert-PathDoesNotContainDotDot -Path $s -Context "requiredBuildOutputFiles[$index]" -ManifestPath $ManifestPath
      }

      $key = $s.Replace('\', '/').ToLowerInvariant()
      if ($seen.ContainsKey($key)) {
        throw "Invalid manifest '$ManifestPath': requiredBuildOutputFiles contains a duplicate entry '$s'."
      }
      $seen[$key] = $true
    }
  }

  $toolProp = $data.PSObject.Properties['toolFiles']
  if ($null -ne $toolProp) {
    Assert-JsonArray -Value $toolProp.Value -Context 'toolFiles' -ManifestPath $ManifestPath

    $seen = @{}
    $index = 0
    foreach ($entry in $toolProp.Value) {
      $index++
      Assert-JsonString -Value $entry -Context "toolFiles[$index]" -ManifestPath $ManifestPath
      $s = ([string]$entry).Trim()

      if ([System.IO.Path]::GetExtension($s).ToLowerInvariant() -ne '.exe') {
        throw "Invalid manifest '$ManifestPath': toolFiles[$index] '$s' must end with '.exe'."
      }

      if ($DriverRoot) {
        Assert-PathIsRelativeAndUnderRoot -Root $DriverRoot -ChildPath $s -Context "toolFiles[$index]" -ManifestPath $ManifestPath
      } elseif ([System.IO.Path]::IsPathRooted($s)) {
        throw "Invalid manifest '$ManifestPath': toolFiles[$index] must be a relative path (got '$s')."
      } else {
        Assert-PathDoesNotContainDotDot -Path $s -Context "toolFiles[$index]" -ManifestPath $ManifestPath
      }

      $key = $s.Replace('\', '/').ToLowerInvariant()
      if ($seen.ContainsKey($key)) {
        throw "Invalid manifest '$ManifestPath': toolFiles contains a duplicate entry '$s'."
      }
      $seen[$key] = $true
    }
  }
  $wdfProp = $data.PSObject.Properties['wdfCoInstaller']
  if ($null -ne $wdfProp) {
    Assert-JsonObject -Value $wdfProp.Value -Context 'wdfCoInstaller' -ManifestPath $ManifestPath
    $wdf = $wdfProp.Value

    $allowedWdfKeys = @('kmdfVersion', 'dllName')
    $unknownWdfKeys = @()
    foreach ($prop in $wdf.PSObject.Properties) {
      if ($allowedWdfKeys -cnotcontains $prop.Name) {
        $unknownWdfKeys += $prop.Name
      }
    }
    if ($unknownWdfKeys.Count -gt 0) {
      $sorted = $unknownWdfKeys | Sort-Object
      throw "Invalid manifest '$ManifestPath': wdfCoInstaller has unknown key(s): $($sorted -join ', '). Allowed keys: $($allowedWdfKeys -join ', ')"
    }

    $kmdfProp = $wdf.PSObject.Properties['kmdfVersion']
    if ($null -eq $kmdfProp) {
      throw "Invalid manifest '$ManifestPath': wdfCoInstaller.kmdfVersion is required."
    }
    Assert-JsonString -Value $kmdfProp.Value -Context 'wdfCoInstaller.kmdfVersion' -ManifestPath $ManifestPath
    $kmdfVersion = ([string]$kmdfProp.Value).Trim()
    if ($kmdfVersion -notmatch '^\d+\.\d+$') {
      throw "Invalid manifest '$ManifestPath': wdfCoInstaller.kmdfVersion '$kmdfVersion' must match 'major.minor' (example: 1.11)."
    }

    $dllNameProp = $wdf.PSObject.Properties['dllName']
    if ($null -ne $dllNameProp) {
      Assert-JsonString -Value $dllNameProp.Value -Context 'wdfCoInstaller.dllName' -ManifestPath $ManifestPath
      $dllName = ([string]$dllNameProp.Value).Trim()

      if ($dllName.IndexOfAny(@([char]'\', [char]'/')) -ge 0) {
        throw "Invalid manifest '$ManifestPath': wdfCoInstaller.dllName must be a file name, not a path."
      }

      if ($dllName -notmatch '^WdfCoInstaller\d{5}\.dll$') {
        throw "Invalid manifest '$ManifestPath': wdfCoInstaller.dllName must match 'WdfCoInstallerNNNNN.dll' (example: WdfCoInstaller01011.dll)."
      }
    }
  }

  return $data
}

Export-ModuleMember -Function @(
  'Validate-DriverPackageManifest'
)
