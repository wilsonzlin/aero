param(
    # Optional: IP/hostname to ping for the network smoke test.
    # If omitted, the script will ping the default gateway (if present).
    [string]$PingTarget = "",

    # Optional: attempt to play a system .wav using System.Media.SoundPlayer.
    [switch]$PlayTestSound,

    # Optional: run extended aerogpu_dbgctl diagnostics (best-effort) and embed output in the report.
    # Note: verify.ps1 will still attempt a safe dbgctl --help / /? capture when dbgctl is present;
    # this switch enables the more detailed --status run (when AeroGPU is detected and healthy).
    [switch]$RunDbgctl,

    # Optional: run aerogpu_dbgctl.exe --selftest (requires AeroGPU to be present and healthy).
    # This is off by default because selftest may report GPU_BUSY on active desktops; use it when
    # explicitly gathering GPU bring-up diagnostics.
    [switch]$RunDbgctlSelftest
)

# PowerShell 2.0 compatible (Windows 7 inbox).

$global:ErrorActionPreference = "Continue"

function Get-IsAdmin {
    try {
        $currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = New-Object Security.Principal.WindowsPrincipal($currentIdentity)
        return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    } catch {
        return $false
    }
}

function Merge-Status([string]$a, [string]$b) {
    # Priority: FAIL > WARN > PASS
    if ($a -eq "FAIL" -or $b -eq "FAIL") { return "FAIL" }
    if ($a -eq "WARN" -or $b -eq "WARN") { return "WARN" }
    return "PASS"
}

function Invoke-Capture([string]$file, [string[]]$args) {
    $out = ""
    $exit = 0
    try {
        $out = & $file @args 2>&1 | Out-String
        $exit = $LASTEXITCODE
    } catch {
        $out = $_.Exception.Message
        $exit = 1
    }
    return @{
        exit_code = $exit
        output = $out
    }
}

function Get-TextExcerpt([string]$text, [int]$maxLines, [int]$maxChars) {
    if (-not $text) { return "" }
    $t = "" + $text
    if ($maxChars -gt 0 -and $t.Length -gt $maxChars) {
        $t = $t.Substring(0, $maxChars)
    }
    $lines = $t -split "`r?`n"
    $out = @()
    $count = 0
    foreach ($line in $lines) {
        if ($count -ge $maxLines) { break }
        $s = ("" + $line).TrimEnd()
        if ($s.Length -eq 0) { continue }
        $out += $s
        $count++
    }
    return ($out -join "`r`n")
}

function Find-AeroGpuDbgctl([string]$scriptDir, [bool]$is64) {
    $searched = @()

    # Prefer a binary matching the OS bitness, but fall back to the other one (x86 runs under WOW64).
    $preferred = @()
    $fallback = @()

    $preferred += (Join-Path $scriptDir "aerogpu_dbgctl.exe")
    $fallback += (Join-Path $scriptDir "aerogpu_dbgctl.exe")

    # Optional tools payload (may be packaged under tools\).
    $preferred += (Join-Path $scriptDir "tools\aerogpu_dbgctl.exe")
    $fallback += (Join-Path $scriptDir "tools\aerogpu_dbgctl.exe")

    if ($is64) {
        $preferred += (Join-Path $scriptDir "tools\amd64\aerogpu_dbgctl.exe")
        $preferred += (Join-Path $scriptDir "tools\x64\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "tools\x86\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "tools\i386\aerogpu_dbgctl.exe")
  
        # Packaged path (AeroGPU driver payload).
        $preferred += (Join-Path $scriptDir "drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe")

        # Legacy packaged paths (older Guest Tools media).
        $preferred += (Join-Path $scriptDir "drivers\amd64\aerogpu\tools\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\x86\aerogpu\tools\aerogpu_dbgctl.exe")

        $preferred += (Join-Path $scriptDir "drivers\amd64\aerogpu\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\x86\aerogpu\aerogpu_dbgctl.exe")
    } else {
        $preferred += (Join-Path $scriptDir "tools\x86\aerogpu_dbgctl.exe")
        $preferred += (Join-Path $scriptDir "tools\i386\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "tools\amd64\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "tools\x64\aerogpu_dbgctl.exe")
  
        # Packaged path (AeroGPU driver payload).
        $preferred += (Join-Path $scriptDir "drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe")

        # Legacy packaged paths (older Guest Tools media).
        $preferred += (Join-Path $scriptDir "drivers\x86\aerogpu\tools\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\amd64\aerogpu\tools\aerogpu_dbgctl.exe")

        $preferred += (Join-Path $scriptDir "drivers\x86\aerogpu\aerogpu_dbgctl.exe")
        $fallback += (Join-Path $scriptDir "drivers\amd64\aerogpu\aerogpu_dbgctl.exe")
    }

    foreach ($p in $preferred) { $searched += $p }
    foreach ($p in $fallback) { $searched += $p }

    foreach ($p in $preferred) {
        if (Test-Path $p) {
            return @{ found = $true; path = $p; searched = $searched }
        }
    }
    foreach ($p in $fallback) {
        if (Test-Path $p) {
            return @{ found = $true; path = $p; searched = $searched }
        }
    }

    # Last resort: recursive search under tools\ (layout may vary between packagers).
    $toolsDir = Join-Path $scriptDir "tools"
    if (Test-Path $toolsDir) {
        try {
            $searched += ($toolsDir + " (recursive search)")
            $hit = Get-ChildItem -Path $toolsDir -Recurse -Filter aerogpu_dbgctl.exe -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($hit -and $hit.FullName) {
                return @{ found = $true; path = ("" + $hit.FullName); searched = $searched }
            }
        } catch { }
    }

    return @{ found = $false; path = $null; searched = $searched }
}

function Join-CommandLineArgs([string[]]$args) {
    # Minimal Windows command-line argument quoting (sufficient for our usage).
    $parts = @()
    if ($args) {
        foreach ($a in $args) {
            if ($a -eq $null) { continue }
            $s = "" + $a
            if ($s -match '[\s"]') {
                $s = '"' + ($s -replace '"', '\"') + '"'
            }
            $parts += $s
        }
    }
    return ($parts -join " ")
}

function Invoke-CaptureWithTimeout([string]$file, [string[]]$args, [int]$timeoutMs) {
    # PowerShell 2.0-compatible stdout/stderr capture with a hard timeout.
    $stdout = ""
    $stderr = ""
    $exit = $null
    $timedOut = $false
    try {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $file
        $psi.Arguments = Join-CommandLineArgs $args
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true

        $p = New-Object System.Diagnostics.Process
        $p.StartInfo = $psi
        [void]$p.Start()

        $exited = $true
        if ($timeoutMs -and $timeoutMs -gt 0) {
            $exited = $p.WaitForExit($timeoutMs)
            if (-not $exited) {
                $timedOut = $true
                try { $p.Kill() } catch { }
                try { $exited = $p.WaitForExit(1000) } catch { $exited = $false }
            }
        } else {
            [void]$p.WaitForExit()
        }

        if (-not $exited) {
            # Avoid blocking on ReadToEnd if the process failed to terminate.
            $stderr = "Timed out (process did not exit after kill)."
        } else {
            try { $stdout = $p.StandardOutput.ReadToEnd() } catch { $stdout = "" }
            try { $stderr = $p.StandardError.ReadToEnd() } catch { $stderr = "" }
            if (-not $timedOut) {
                try { $exit = $p.ExitCode } catch { $exit = $null }
            }
        }
    } catch {
        $stderr = $_.Exception.Message
        $exit = 1
    }

    return @{
        exit_code = $exit
        stdout = $stdout
        stderr = $stderr
        timed_out = $timedOut
    }
}

function Parse-CmdQuotedList([string]$value) {
    # Parse a CMD-style list of quoted values:
    #   "A" "B" "C"
    $items = @()
    if (-not $value -or $value.Length -eq 0) { return $items }
    $matches = [regex]::Matches($value, '"([^"]+)"')
    foreach ($m in $matches) {
        $items += $m.Groups[1].Value
    }
    return $items
}

function Build-HwidPrefixRegex([string[]]$hwids) {
    # Returns a case-insensitive regex that matches Win32_PnPEntity.PNPDeviceID
    # starting with one of the configured PCI HWIDs (prefix match).
    if (-not $hwids -or $hwids.Count -eq 0) { return $null }
    $parts = @()
    foreach ($h in $hwids) {
        if (-not $h -or $h.Length -eq 0) { continue }
        $parts += [regex]::Escape($h)
    }
    if ($parts.Count -eq 0) { return $null }
    return "(?i)^(" + ($parts -join "|") + ")"
}

function Dedup-CaseInsensitive([string[]]$items) {
    $out = @()
    if (-not $items) { return $out }
    foreach ($s in $items) {
        if (-not $s) { continue }
        $t = ("" + $s).Trim()
        if ($t.Length -eq 0) { continue }
        $exists = $false
        foreach ($e in $out) {
            if ($e.ToLower() -eq $t.ToLower()) { $exists = $true; break }
        }
        if (-not $exists) { $out += $t }
    }
    return $out
}

function Load-GuestToolsConfig([string]$scriptDir) {
    $cfgFile = Join-Path (Join-Path $scriptDir "config") "devices.cmd"
    $result = @{
        found = $false
        file_path = $cfgFile
        exit_code = $null
        raw = $null
        vars = @{}
    }

    if (-not (Test-Path $cfgFile)) { return $result }

    $result.found = $true
    $cmd = 'call "' + $cfgFile + '" >nul 2>&1 & set AERO_'
    $cap = Invoke-Capture "cmd.exe" @("/c", $cmd)
    $result.exit_code = $cap.exit_code
    $result.raw = $cap.output

    if ($cap.exit_code -ne 0 -or -not $cap.output) { return $result }

    foreach ($line in $cap.output -split "`r?`n") {
        $t = $line.Trim()
        if ($t -match '^([A-Za-z0-9_]+)=(.*)$') {
            $result.vars[$matches[1]] = $matches[2]
        }
    }

    return $result
}

function Try-GetWmi([string]$class, [string]$filter) {
    try {
        if ($filter -and $filter.Length -gt 0) {
            return Get-WmiObject -Class $class -Filter $filter
        }
        return Get-WmiObject -Class $class
    } catch {
        return $null
    }
}

function Get-RegistryDword([string]$path, [string]$name) {
    try {
        $item = Get-ItemProperty -Path $path -ErrorAction Stop
        return $item.$name
    } catch {
        return $null
    }
}

function Get-RegistryString([string]$path, [string]$name) {
    try {
        $item = Get-ItemProperty -Path $path -ErrorAction Stop
        $v = $item.$name
        if ($v -eq $null) { return $null }
        return "" + $v
    } catch {
        return $null
    }
}

function Resolve-DisplayAdapterRegistryKey([string]$pnpDeviceId) {
    # Best-effort mapping from a PNP device instance ID (e.g. PCI\VEN_...&DEV_...\...)
    # to the registry key where display miniport driver parameters are commonly stored.
    #
    # Preferred: display class key referenced by Enum\<pnp>\Driver:
    #   HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4D36E968-E325-11CE-BFC1-08002BE10318}\XXXX
    #
    # Fallback: Control\Video instance key (some stacks read config from here):
    #   HKLM:\SYSTEM\CurrentControlSet\Control\Video\{VideoID}\0000
    #
    # This is used to surface informational AeroGPU driver settings such as:
    #   HKR\Parameters\NonLocalMemorySizeMB (REG_DWORD)
    $out = @{
        pnp_device_id = $pnpDeviceId
        enum_key_path = $null
        enum_key_exists = $false
        driver_value = $null
        display_class_key_path = $null
        display_class_key_exists = $false
        video_id = $null
        control_video_base_path = $null
        control_video_key_path = $null
        control_video_key_exists = $false
        resolved_key_path = $null
        resolved_key_kind = $null # display_class | control_video
    }

    $pnp = $null
    if ($pnpDeviceId) { $pnp = ("" + $pnpDeviceId).Trim() }
    if (-not $pnp -or $pnp.Length -eq 0) { return $out }

    # 1) Enum -> Driver -> Control\Class\{GUID}\XXXX (best-effort)
    $enumKey = "HKLM:\SYSTEM\CurrentControlSet\Enum\" + $pnp
    $out.enum_key_path = $enumKey
    $out.enum_key_exists = (Test-Path $enumKey)
    if ($out.enum_key_exists) {
        $drv = Get-RegistryString $enumKey "Driver"
        if ($drv -and $drv.Trim().Length -gt 0) {
            $out.driver_value = $drv
            $classKey = "HKLM:\SYSTEM\CurrentControlSet\Control\Class\" + $drv
            $out.display_class_key_path = $classKey
            $out.display_class_key_exists = (Test-Path $classKey)
            if ($out.display_class_key_exists) {
                $out.resolved_key_path = $classKey
                $out.resolved_key_kind = "display_class"

                # If present, VideoID can be used to locate Control\Video key.
                $vid = Get-RegistryString $classKey "VideoID"
                if ($vid -and $vid.Trim().Length -gt 0) {
                    $out.video_id = $vid
                    $videoBase = "HKLM:\SYSTEM\CurrentControlSet\Control\Video\" + $vid
                    $out.control_video_base_path = $videoBase
                    if (Test-Path $videoBase) {
                        $candidate = Join-Path $videoBase "0000"
                        if (-not (Test-Path $candidate)) {
                            # Fall back to the first numeric subkey (0000, 0001, ...).
                            $children = $null
                            try { $children = Get-ChildItem -Path $videoBase -ErrorAction Stop } catch { $children = $null }
                            if ($children) {
                                $names = @()
                                foreach ($ch in $children) {
                                    $n = "" + $ch.PSChildName
                                    if ($n -match '^\d{4}$') { $names += $n }
                                }
                                if ($names.Count -gt 0) {
                                    $sorted = $names | Sort-Object
                                    $candidate = Join-Path $videoBase $sorted[0]
                                }
                            }
                        }
                        $out.control_video_key_path = $candidate
                        $out.control_video_key_exists = (Test-Path $candidate)
                    }
                }
            }
        }
    }

    # 2) Fallback scan: Control\Video -> *\0000 -> DeviceInstanceID == PNPDeviceID
    # If we already found a Control\Video key via VideoID, skip the scan.
    if (-not $out.control_video_key_path) {
        $videoRoot = "HKLM:\SYSTEM\CurrentControlSet\Control\Video"
        if (Test-Path $videoRoot) {
            $guidKeys = $null
            try { $guidKeys = Get-ChildItem -Path $videoRoot -ErrorAction Stop } catch { $guidKeys = $null }
            if ($guidKeys) {
                foreach ($g in $guidKeys) {
                    $guidName = "" + $g.PSChildName
                    if (-not $guidName -or $guidName.Length -eq 0) { continue }
                    $gPath = Join-Path $videoRoot $guidName
                    $sub = $null
                    try { $sub = Get-ChildItem -Path $gPath -ErrorAction Stop } catch { $sub = $null }
                    if (-not $sub) { continue }
                    foreach ($s in $sub) {
                        $childName = "" + $s.PSChildName
                        if (-not ($childName -match '^\d{4}$')) { continue }
                        $sPath = Join-Path $gPath $childName
                        $devInst = Get-RegistryString $sPath "DeviceInstanceID"
                        if (-not $devInst) { continue }
                        if ($devInst.Trim().ToLower() -eq $pnp.ToLower()) {
                            $out.video_id = $guidName
                            $out.control_video_base_path = $gPath
                            $out.control_video_key_path = $sPath
                            $out.control_video_key_exists = $true
                            if (-not $out.resolved_key_path) {
                                $out.resolved_key_path = $sPath
                                $out.resolved_key_kind = "control_video"
                            }
                            break
                        }
                    }
                    if ($out.control_video_key_path) { break }
                }
            }
        }
    }

    return $out
}

function StartType-FromStartValue($startValue) {
    # https://learn.microsoft.com/en-us/windows-hardware/drivers/install/inf-addservice-directive
    switch ($startValue) {
        0 { return "BOOT_START" }
        1 { return "SYSTEM_START" }
        2 { return "AUTO_START" }
        3 { return "DEMAND_START" }
        4 { return "DISABLED" }
        default { return $null }
    }
}

function Get-ConfigManagerErrorMeaning($code) {
    # Common subset of ConfigManagerErrorCode meanings for quick triage.
    # See: https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-pnpentity
    switch ($code) {
        0 { return "OK" }
        1 { return "Device is not configured correctly" }
        10 { return "Device cannot start" }
        12 { return "Not enough resources" }
        14 { return "Device cannot work properly until restarted" }
        18 { return "Drivers must be reinstalled" }
        19 { return "Registry configuration is incomplete or damaged" }
        22 { return "Device is disabled" }
        24 { return "Device is not present / not working properly / drivers not installed" }
        28 { return "Drivers for this device are not installed" }
        31 { return "Device is not working properly; Windows cannot load required drivers" }
        32 { return "Driver (service) is disabled" }
        37 { return "Windows cannot initialize the device driver" }
        39 { return "Windows cannot load the device driver (driver may be corrupted/missing)" }
        43 { return "Windows has stopped this device (it reported problems)" }
        52 { return "Windows cannot verify the digital signature for the drivers (Code 52)" }
        default { return $null }
    }
}

function Add-DeviceBindingCheck(
    [string]$key,
    [string]$title,
    [object[]]$devices,
    [string[]]$serviceCandidates,
    [string[]]$pnpClassCandidates,
    [string]$pnpIdRegex,
    [string[]]$nameKeywords,
    [string]$missingSummary
) {
    $matches = @()
    foreach ($d in $devices) {
        $svc = "" + $d.service
        $name = "" + $d.name
        $mfr = "" + $d.manufacturer
        $cls = "" + $d.pnp_class
        $pnpid = "" + $d.pnp_device_id

        $match = $false
        if ($svc -and $svc.Length -gt 0 -and $serviceCandidates -and $serviceCandidates.Count -gt 0) {
            foreach ($c in $serviceCandidates) {
                if ($svc.ToLower() -eq $c.ToLower()) { $match = $true; break }
            }
        }

        if (-not $match -and $pnpClassCandidates -and $pnpClassCandidates.Count -gt 0) {
            $classMatch = $false
            foreach ($pc in $pnpClassCandidates) {
                if ($cls -and ($cls.ToLower() -eq $pc.ToLower())) { $classMatch = $true; break }
            }

            if ($classMatch) {
                if ($pnpIdRegex -and $pnpid -match $pnpIdRegex) { $match = $true }
                if (-not $match -and $nameKeywords -and $nameKeywords.Count -gt 0) {
                    $lower = ($name + " " + $mfr).ToLower()
                    foreach ($kw in $nameKeywords) {
                        if ($lower.Contains($kw.ToLower())) { $match = $true; break }
                    }
                }
            }
        }

        if ($match) { $matches += $d }
    }

    $data = @{
        service_candidates = $serviceCandidates
        pnp_class_candidates = $pnpClassCandidates
        pnp_id_regex = $pnpIdRegex
        matched_devices = @()
    }
    $details = @()
    if ($serviceCandidates -and $serviceCandidates.Count -gt 0) { $details += "Service candidates: " + ($serviceCandidates -join ", ") }
    if ($pnpClassCandidates -and $pnpClassCandidates.Count -gt 0) { $details += "PNP class candidates: " + ($pnpClassCandidates -join ", ") }
    if ($pnpIdRegex) { $details += "PNP ID regex: " + $pnpIdRegex }
    if ($nameKeywords -and $nameKeywords.Count -gt 0) { $details += "Name keywords: " + ($nameKeywords -join ", ") }

    foreach ($m in $matches) {
        $inf = $null
        $signer = $null
        if ($m.signed_driver) {
            $inf = "" + $m.signed_driver.inf_name
            $signer = "" + $m.signed_driver.signer
        }

        $data.matched_devices += @{
            name = "" + $m.name
            manufacturer = "" + $m.manufacturer
            pnp_device_id = "" + $m.pnp_device_id
            pnp_class = "" + $m.pnp_class
            service = "" + $m.service
            status = "" + $m.status
            config_manager_error_code = $m.config_manager_error_code
            config_manager_error_meaning = "" + $m.config_manager_error_meaning
            inf_name = $inf
            signer = $signer
        }

        $line = "" + $m.name
        if ($m.service) { $line += " (service=" + $m.service + ")" }
        if ($m.config_manager_error_code -ne $null) {
            $line += ", CM=" + $m.config_manager_error_code
            if ($m.config_manager_error_meaning) { $line += " (" + $m.config_manager_error_meaning + ")" }
        }
        if ($inf) { $line += ", INF=" + $inf }
        if ($signer) { $line += ", Signer=" + $signer }
        $details += $line
    }

    if ($matches.Count -eq 0) {
        if ($key -eq "device_binding_storage") {
            $details += "See: docs/windows7-driver-troubleshooting.md#issue-storage-controller-switch-gotchas-boot-loops-0x7b"
        } else {
            $details += "See: docs/windows7-driver-troubleshooting.md#issue-virtio-device-not-found-or-unknown-device-after-switching"
        }
        Add-Check $key $title "WARN" $missingSummary $data $details
        return
    }

    $ok = @($matches | Where-Object { $_.config_manager_error_code -eq 0 }).Count
    $bad = @($matches | Where-Object { $_.config_manager_error_code -ne $null -and $_.config_manager_error_code -ne 0 }).Count

    $status = "PASS"
    $summary = "Matched devices: " + $matches.Count + " (OK: " + $ok + ", Problem: " + $bad + ")"
    if ($bad -gt 0 -and $ok -gt 0) { $status = "WARN" }
    if ($bad -gt 0 -and $ok -eq 0) { $status = "FAIL" }

    if ($bad -gt 0) {
        $codes = @{}
        foreach ($m in $matches) {
            if ($m.config_manager_error_code -ne $null -and $m.config_manager_error_code -ne 0) {
                $codes[$m.config_manager_error_code.ToString()] = $true
            }
        }
        if ($codes.ContainsKey("52")) { $details += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-52-signature-and-trust-failures" }
        if ($codes.ContainsKey("28")) { $details += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-28-drivers-not-installed" }
        if ($codes.ContainsKey("10")) { $details += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-10-device-cannot-start" }
    }

    Add-Check $key $title $status $summary $data $details
}

function Load-CertsFromFile([string]$path) {
    # Supports:
    # - .cer/.crt (single X509Certificate2)
    # - .p7b (PKCS#7 container with one or more certificates)
    $ext = ""
    try { $ext = [System.IO.Path]::GetExtension($path).ToLower() } catch { $ext = "" }

    if ($ext -eq ".p7b") {
        try {
            $coll = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2Collection
            $coll.Import($path)
            $out = @()
            foreach ($c in $coll) { $out += $c }
            return $out
        } catch {
            return @()
        }
    }

    try {
        $c = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($path)
        return @($c)
    } catch {
        return @()
    }
}

function Find-CertInStore([string]$thumbprint, [string]$storeName, [string]$storeLocation) {
    try {
        $loc = [System.Security.Cryptography.X509Certificates.StoreLocation]::$storeLocation
        $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($storeName, $loc)
        $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
        foreach ($cert in $store.Certificates) {
            if ($cert.Thumbprint -and ($cert.Thumbprint.ToUpper() -eq $thumbprint.ToUpper())) {
                $store.Close()
                return $true
            }
        }
        $store.Close()
        return $false
    } catch {
        return $false
    }
}

function ConvertTo-JsonCompat($obj) {
    try {
        [void][System.Reflection.Assembly]::LoadWithPartialName("System.Web.Extensions")
        $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
        # Report can include raw pnputil output; bump limit above default.
        $serializer.MaxJsonLength = 104857600
        return $serializer.Serialize($obj)
    } catch {
        # Minimal fallback: emit a tiny error JSON rather than failing entirely.
        $msg = $_.Exception.Message
        $msg = $msg -replace '\\', '\\\\'
        $msg = $msg -replace '"', '\"'
        return "{""error"":""Failed to serialize JSON: $msg""}"
    }
}

function Parse-JsonCompat([string]$json) {
    try {
        [void][System.Reflection.Assembly]::LoadWithPartialName("System.Web.Extensions")
        $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
        $serializer.MaxJsonLength = 104857600
        return $serializer.DeserializeObject($json)
    } catch {
        return $null
    }
}

function Format-Json([string]$json) {
    # Tiny JSON pretty-printer compatible with PowerShell 2.0.
    # Assumes input is valid JSON with no comments.
    $indent = 0
    $inString = $false
    $escape = $false
    $sb = New-Object System.Text.StringBuilder
    foreach ($ch in $json.ToCharArray()) {
        if ($escape) {
            [void]$sb.Append($ch)
            $escape = $false
            continue
        }
        if ($ch -eq '\') {
            [void]$sb.Append($ch)
            if ($inString) { $escape = $true }
            continue
        }
        if ($ch -eq '"') {
            [void]$sb.Append($ch)
            $inString = -not $inString
            continue
        }
        if (-not $inString) {
            switch ($ch) {
                '{' {
                    [void]$sb.Append("{`r`n")
                    $indent++
                    [void]$sb.Append(("  " * $indent))
                    continue
                }
                '}' {
                    [void]$sb.Append("`r`n")
                    $indent = [Math]::Max(0, $indent - 1)
                    [void]$sb.Append(("  " * $indent))
                    [void]$sb.Append("}")
                    continue
                }
                '[' {
                    [void]$sb.Append("[`r`n")
                    $indent++
                    [void]$sb.Append(("  " * $indent))
                    continue
                }
                ']' {
                    [void]$sb.Append("`r`n")
                    $indent = [Math]::Max(0, $indent - 1)
                    [void]$sb.Append(("  " * $indent))
                    [void]$sb.Append("]")
                    continue
                }
                ',' {
                    [void]$sb.Append(",`r`n")
                    [void]$sb.Append(("  " * $indent))
                    continue
                }
                ':' {
                    [void]$sb.Append(": ")
                    continue
                }
                default {
                    if ($ch -match '\s') { continue }
                }
            }
        }
        [void]$sb.Append($ch)
    }
    return $sb.ToString()
}

function Get-FileSha256Hex([string]$path) {
    # PowerShell 2.0-compatible SHA-256 helper (Win7 inbox lacks Get-FileHash).
    try {
        $stream = [System.IO.File]::OpenRead($path)
        try {
            $sha = New-Object System.Security.Cryptography.SHA256Managed
            try {
                $hash = $sha.ComputeHash($stream)
            } finally {
                try { $sha.Dispose() } catch { }
            }
        } finally {
            try { $stream.Dispose() } catch { }
        }

        $sb = New-Object System.Text.StringBuilder
        foreach ($b in $hash) {
            [void]$sb.AppendFormat("{0:x2}", $b)
        }
        return $sb.ToString()
    } catch {
        return $null
    }
}

function Read-TextFileWithEncodingDetection([string]$path) {
    # PowerShell 2.0-compatible text reader with BOM + UTF-16 heuristic detection.
    #
    # Why: Some driver INFs ship as UTF-16LE/BE without BOM. Get-Content treats BOM-less
    # files as ANSI, which makes INF parsing miss AddService/HWIDs (and breaks media
    # inventory/correlation).
    #
    # Behavior:
    # - Detect BOM: UTF-8, UTF-16LE, UTF-16BE.
    # - Heuristic detect BOM-less UTF-16: if lots of NUL bytes in either even or odd
    #   positions (sampled), treat as UTF-16. Prefer LE unless strong evidence for BE.
    # - Fallback: UTF-8/ASCII for BOM-less non-UTF16 text.
    # - Strip any leading U+FEFF (BOM char) after decoding.
    $bytes = $null
    try {
        $bytes = [System.IO.File]::ReadAllBytes($path)
    } catch {
        throw
    }

    if (-not $bytes) { return "" }

    $enc = $null
    $offset = 0

    # BOM detection.
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        $enc = [System.Text.Encoding]::UTF8
        $offset = 3
    } elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) {
        # UTF-16LE
        $enc = [System.Text.Encoding]::Unicode
        $offset = 2
    } elseif ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) {
        # UTF-16BE
        $enc = [System.Text.Encoding]::BigEndianUnicode
        $offset = 2
    } else {
        # BOM-less heuristic for UTF-16.
        if (($bytes.Length % 2) -eq 0 -and $bytes.Length -ge 2) {
            $evenZero = 0
            $oddZero = 0

            # Sample up to 4KiB for speed; INF files are small but keep this bounded.
            $sampleLen = $bytes.Length
            if ($sampleLen -gt 4096) { $sampleLen = 4096 }

            $i = 0
            while ($i -lt $sampleLen) {
                if ($bytes[$i] -eq 0) { $evenZero++ }
                if ($bytes[$i + 1] -eq 0) { $oddZero++ }
                $i += 2
            }

            $pairs = ($sampleLen / 2)
            if ($pairs -gt 0) {
                $evenFrac = ($evenZero / [double]$pairs)
                $oddFrac = ($oddZero / [double]$pairs)

                # "Large fraction" threshold; tuned for typical BOM-less UTF-16 INFs
                # where ASCII dominates (so every other byte is NUL).
                $nulThreshold = 0.30

                if ($evenFrac -ge $nulThreshold -or $oddFrac -ge $nulThreshold) {
                    # If even bytes are *significantly* more likely to be NUL, assume BE.
                    # Otherwise default to LE.
                    if ($evenFrac -gt ($oddFrac + 0.15) -and $evenFrac -gt 0.40) {
                        $enc = [System.Text.Encoding]::BigEndianUnicode
                    } else {
                        $enc = [System.Text.Encoding]::Unicode
                    }
                }
            }
        }

        if (-not $enc) {
            # Default to UTF-8/ASCII for BOM-less non-UTF16 text.
            # (ASCII is a subset of UTF-8; for legacy ANSI INFs this still preserves all ASCII tokens
            # like [Version]/AddService/HWIDs which are what we parse.)
            $enc = [System.Text.Encoding]::UTF8
        }
    }

    $text = $null
    if ($offset -gt 0) {
        $text = $enc.GetString($bytes, $offset, ($bytes.Length - $offset))
    } else {
        $text = $enc.GetString($bytes)
    }

    # Strip any leading BOM char that may have been decoded.
    try {
        if ($text -and $text.Length -gt 0 -and ([int]$text[0] -eq 0xFEFF)) {
            $text = $text.Substring(1)
        }
    } catch { }

    return $text
}

function Strip-InfComment([string]$line) {
    # Strip `;` comments while preserving semicolons inside quotes (best-effort).
    if ($line -eq $null) { return $null }
    $s = "" + $line
    $inQuote = $false
    for ($i = 0; $i -lt $s.Length; $i++) {
        $ch = $s[$i]
        if ($ch -eq '"') { $inQuote = -not $inQuote; continue }
        if (($ch -eq ';') -and (-not $inQuote)) {
            return $s.Substring(0, $i)
        }
    }
    return $s
}

function Resolve-InfStringValue([string]$value, [hashtable]$strings) {
    # Best-effort expansion for values like %Foo% using the INF [Strings] section.
    if (-not $value) { return $null }
    $t = ("" + $value).Trim()
    $t = $t.Trim('"')
    $m = [regex]::Match($t, '^\%([^%]+)\%$')
    if ($m.Success -and $strings -and $strings.ContainsKey($m.Groups[1].Value)) {
        return "" + $strings[$m.Groups[1].Value]
    }
    return $t
}

function Parse-InfMetadata([string]$path) {
    # Best-effort INF parser (sufficient for Win7 virtio/Aero driver triage).
    # Extracts:
    # - Provider (resolved from [Strings] if possible)
    # - Class
    # - DriverVer (raw + split date/version if parseable)
    # - Hardware ID patterns (prefix match candidates; e.g. PCI\VEN_...&DEV_....)
    # - AddService names (useful for boot-critical storage correlation)
    $out = @{
        inf_path = $path
        inf_file = $null
        provider = $null
        provider_raw = $null
        class = $null
        driver_ver_raw = $null
        driver_date = $null
        driver_version = $null
        hwid_prefixes = @()
        add_services = @()
        parse_errors = @()
    }

    try { $out.inf_file = [System.IO.Path]::GetFileName($path) } catch { $out.inf_file = $path }

    $lines = $null
    try {
        $raw = Read-TextFileWithEncodingDetection $path
        # Split like Get-Content would (support CRLF/LF/CR).
        $lines = $raw -split "\r\n|\n|\r"
    } catch {
        $out.parse_errors += ("Failed to read: " + $_.Exception.Message)
        return $out
    }

    $section = ""
    $strings = @{}
    $hwids = @{}
    $services = @{}

    foreach ($line in $lines) {
        if ($line -eq $null) { continue }
        $t = ("" + (Strip-InfComment ("" + $line))).Trim()
        if ($t.Length -eq 0) { continue }

        # Some INFs list HWIDs as bare lines (e.g. in a [HardwareIds] section) without commas.
        $bare = $t.Trim('"')
        $mBare = [regex]::Match($bare, '^(?i)((?:PCI|USB|HID|ACPI|ROOT|SW)\\[^\s,;"=]+)')
        if ($mBare.Success) {
            $hw = $mBare.Groups[1].Value
            $hwids[$hw.ToUpper()] = $hw
        }

        if ($t -match '^\[(.+)\]$') {
            $section = $matches[1].Trim()
            continue
        }

        if ($section -match '(?i)^strings$') {
            if ($t -match '^\s*([^=]+?)\s*=\s*(.+?)\s*$') {
                $k = $matches[1].Trim()
                $v = $matches[2].Trim()
                $v = $v.Trim('"')
                if ($k.Length -gt 0) { $strings[$k] = $v }
            }
            continue
        }

        if ($section -match '(?i)^version$') {
            if ($t -match '^(?i)Provider\s*=\s*(.+)$') { $out.provider_raw = $matches[1].Trim(); continue }
            if ($t -match '^(?i)Class\s*=\s*(.+)$') { $out.class = $matches[1].Trim(); continue }
            if ($t -match '^(?i)DriverVer\s*=\s*(.+)$') { $out.driver_ver_raw = $matches[1].Trim(); continue }
        }

        # AddService directives can appear in many sections.
        $svcMatch = [regex]::Match($t, '^(?i)AddService\s*=\s*([^,\s]+)')
        if ($svcMatch.Success) {
            $svc = $svcMatch.Groups[1].Value.Trim()
            $svc = $svc.Trim('"')
            if ($svc.Length -gt 0) { $services[$svc.ToLower()] = $svc }
        }

        # Hardware IDs typically appear as the last token(s) after a comma:
        #   %Desc% = InstallSection, PCI\VEN_1AF4&DEV_1041
        if ($t.IndexOf(',') -ge 0) {
            $parts = $t -split ','
            if ($parts -and $parts.Length -ge 2) {
                for ($i = 1; $i -lt $parts.Length; $i++) {
                    $p = ("" + $parts[$i]).Trim()
                    $p = $p.Trim('"')
                    if ($p -match '^(?i)(PCI|USB|HID|ACPI|ROOT|SW)\\') {
                        $hwids[$p.ToUpper()] = $p
                    }
                }
            }
        }
    }

    $out.provider = Resolve-InfStringValue $out.provider_raw $strings

    if ($out.driver_ver_raw) {
        $m = [regex]::Match($out.driver_ver_raw, '^\s*([^,]+)\s*,\s*(.+?)\s*$')
        if ($m.Success) {
            $out.driver_date = $m.Groups[1].Value.Trim()
            $out.driver_version = $m.Groups[2].Value.Trim()
        }
    }

    foreach ($k in $hwids.Keys) { $out.hwid_prefixes += ("" + $hwids[$k]) }
    foreach ($k in $services.Keys) { $out.add_services += ("" + $services[$k]) }

    return $out
}

function Write-TextReport([hashtable]$report, [string]$path) {
    $nl = "`r`n"
    $sb = New-Object System.Text.StringBuilder

    [void]$sb.Append("Aero Guest Tools Verification Report$nl")
    [void]$sb.Append("Generated (UTC): " + $report.tool.ended_utc + $nl)
    [void]$sb.Append("Computer: " + $report.environment.computername + $nl)
    [void]$sb.Append("User: " + $report.environment.username + $nl)
    [void]$sb.Append("Admin: " + $report.environment.is_admin + $nl)
    [void]$sb.Append($nl)
    [void]$sb.Append("OVERALL: " + $report.overall.status + $nl)
    if ($report.overall.summary) {
        [void]$sb.Append($report.overall.summary + $nl)
    }

    $orderedKeys = @(
        "os",
        "guest_tools_manifest",
        "guest_tools_manifest_inputs",
        "extra_files_not_in_manifest",
        "certs_on_media_policy_mismatch",
        "packaged_drivers_summary",
        "optional_tools",
        "guest_tools_setup_state",
        "guest_tools_config",
        "clock_sanity",
        "kb3033929",
        "kb4474419",
        "kb4490628",
        "certificate_store",
        "signature_mode",
        "driver_packages",
        "bound_devices",
        "installed_driver_signatures",
        "installed_driver_binding_summary",
        "device_binding_storage",
        "device_binding_network",
        "device_binding_graphics",
        "aerogpu_umd_files",
        "aerogpu_d3d10_umd_files",
        "aerogpu_dbgctl",
        "device_binding_audio",
        "device_binding_input",
        "virtio_blk_service",
        "virtio_blk_boot_critical",
        "smoke_disk",
        "smoke_network",
        "smoke_graphics",
        "smoke_audio",
        "smoke_input"
    )

    $nonPass = @()
    foreach ($key in $orderedKeys) {
        if (-not $report.checks.ContainsKey($key)) { continue }
        $chk = $report.checks[$key]
        if ($chk.status -ne "PASS") {
            $nonPass += ($chk.status + " - " + $chk.title + " (" + $key + "): " + $chk.summary)
        }
    }
    if ($nonPass.Count -gt 0) {
        [void]$sb.Append($nl + "== Summary (non-PASS checks) ==$nl")
        foreach ($line in $nonPass) {
            [void]$sb.Append("  - " + $line + $nl)
        }
    }

    foreach ($key in $orderedKeys) {
        if (-not $report.checks.ContainsKey($key)) { continue }
        $chk = $report.checks[$key]
        [void]$sb.Append($nl)
        [void]$sb.Append("== " + $chk.title + " ==$nl")
        [void]$sb.Append("Status: " + $chk.status + $nl)
        if ($chk.summary) {
            [void]$sb.Append($chk.summary + $nl)
        }
        if ($chk.details) {
            [void]$sb.Append($nl + "Details:$nl")
            foreach ($line in $chk.details) {
                [void]$sb.Append("  - " + $line + $nl)
            }
        }
    }

    if ($report.errors -and $report.errors.Count -gt 0) {
        [void]$sb.Append($nl + "== Errors ==$nl")
        foreach ($e in $report.errors) {
            [void]$sb.Append("  - " + $e + $nl)
        }
    }
    if ($report.warnings -and $report.warnings.Count -gt 0) {
        [void]$sb.Append($nl + "== Warnings ==$nl")
        foreach ($w in $report.warnings) {
            [void]$sb.Append("  - " + $w + $nl)
        }
    }

    Set-Content -Path $path -Value $sb.ToString() -Encoding UTF8
}

$started = Get-Date
$isAdmin = Get-IsAdmin
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$gtConfig = Load-GuestToolsConfig $scriptDir

$cfgVars = $gtConfig.vars
$cfgVirtioBlkService = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_BLK_SERVICE")) {
    $cfgVirtioBlkService = ("" + $cfgVars["AERO_VIRTIO_BLK_SERVICE"]).Trim()
    if ($cfgVirtioBlkService.Length -eq 0) { $cfgVirtioBlkService = $null }
}
$cfgVirtioNetService = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_NET_SERVICE")) {
    $cfgVirtioNetService = ("" + $cfgVars["AERO_VIRTIO_NET_SERVICE"]).Trim()
    if ($cfgVirtioNetService.Length -eq 0) { $cfgVirtioNetService = $null }
}
$cfgVirtioSndService = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_SND_SERVICE")) {
    $cfgVirtioSndService = ("" + $cfgVars["AERO_VIRTIO_SND_SERVICE"]).Trim()
    if ($cfgVirtioSndService.Length -eq 0) { $cfgVirtioSndService = $null }
}
$cfgVirtioInputService = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_INPUT_SERVICE")) {
    $cfgVirtioInputService = ("" + $cfgVars["AERO_VIRTIO_INPUT_SERVICE"]).Trim()
    if ($cfgVirtioInputService.Length -eq 0) { $cfgVirtioInputService = $null }
}
$cfgGpuService = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_GPU_SERVICE")) {
    $cfgGpuService = ("" + $cfgVars["AERO_GPU_SERVICE"]).Trim()
    if ($cfgGpuService.Length -eq 0) { $cfgGpuService = $null }
}
$cfgVirtioBlkSys = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_BLK_SYS")) {
    $cfgVirtioBlkSys = ("" + $cfgVars["AERO_VIRTIO_BLK_SYS"]).Trim()
    if ($cfgVirtioBlkSys.Length -eq 0) { $cfgVirtioBlkSys = $null }
}
$cfgVirtioSndSys = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_SND_SYS")) {
    $cfgVirtioSndSys = ("" + $cfgVars["AERO_VIRTIO_SND_SYS"]).Trim()
    if ($cfgVirtioSndSys.Length -eq 0) { $cfgVirtioSndSys = $null }
}

$cfgVirtioBlkHwids = @()
$cfgVirtioNetHwids = @()
$cfgVirtioSndHwids = @()
$cfgVirtioInputHwids = @()
$cfgGpuHwids = @()

if ($cfgVars) {
    if ($cfgVars.ContainsKey("AERO_VIRTIO_BLK_HWIDS")) { $cfgVirtioBlkHwids = Parse-CmdQuotedList $cfgVars["AERO_VIRTIO_BLK_HWIDS"] }
    if ($cfgVars.ContainsKey("AERO_VIRTIO_NET_HWIDS")) { $cfgVirtioNetHwids = Parse-CmdQuotedList $cfgVars["AERO_VIRTIO_NET_HWIDS"] }
    if ($cfgVars.ContainsKey("AERO_VIRTIO_SND_HWIDS")) { $cfgVirtioSndHwids = Parse-CmdQuotedList $cfgVars["AERO_VIRTIO_SND_HWIDS"] }
    if ($cfgVars.ContainsKey("AERO_VIRTIO_INPUT_HWIDS")) { $cfgVirtioInputHwids = Parse-CmdQuotedList $cfgVars["AERO_VIRTIO_INPUT_HWIDS"] }
    if ($cfgVars.ContainsKey("AERO_GPU_HWIDS")) { $cfgGpuHwids = Parse-CmdQuotedList $cfgVars["AERO_GPU_HWIDS"] }
}

$cfgVirtioBlkRegex = Build-HwidPrefixRegex $cfgVirtioBlkHwids
$cfgVirtioNetRegex = Build-HwidPrefixRegex $cfgVirtioNetHwids
$cfgVirtioSndRegex = Build-HwidPrefixRegex $cfgVirtioSndHwids
$cfgVirtioInputRegex = Build-HwidPrefixRegex $cfgVirtioInputHwids
$cfgGpuRegex = Build-HwidPrefixRegex $cfgGpuHwids
if (-not $cfgGpuRegex -or ("" + $cfgGpuRegex).Trim().Length -eq 0) {
    # Fallback when Guest Tools config is missing/out-of-date:
    # - AeroGPU: canonical A3A0:0001
    # - virtio-gpu (optional): vendor 1AF4, device 1050 (virtio_device_id=16)
    #
    # Note: older AeroGPU prototype stacks used different vendor IDs; those are deprecated and
    # intentionally not matched here.
    $cfgGpuRegex = '(?i)^(?:PCI\\(?:VEN|VID)_A3A0&(?:DEV|DID)_0001|PCI\\(?:VEN|VID)_1AF4&(?:DEV|DID)_1050)'
}

$outDir = "C:\AeroGuestTools"
$jsonPath = Join-Path $outDir "report.json"
$txtPath = Join-Path $outDir "report.txt"
$storagePreseedSkipMarker = Join-Path $outDir "storage-preseed.skipped.txt"
$storagePreseedSkipped = (Test-Path $storagePreseedSkipMarker)

$report = @{
    schema_version = 1
    tool = @{
         name = "Aero Guest Tools Verify"
         version = "2.5.3"
         started_utc = $started.ToUniversalTime().ToString("o")
         ended_utc = $null
         duration_ms = $null
        script_path = $MyInvocation.MyCommand.Path
        command_line = $MyInvocation.Line
        output_dir = $outDir
        report_json_path = $jsonPath
        report_txt_path = $txtPath
        guest_tools_root = $scriptDir
        guest_tools_config = $gtConfig
    }
    environment = @{
        computername = $env:COMPUTERNAME
        username = $env:USERNAME
        is_admin = $isAdmin
        processor_architecture = $env:PROCESSOR_ARCHITECTURE
    }
    checks = @{}
    warnings = @()
    errors = @()
    overall = @{
        status = "PASS"
        summary = ""
    }
    # Structured, machine-readable summary sections (in addition to per-check data).
    media_integrity = $null
    packaged_drivers_summary = $null
    installed_driver_binding_summary = $null
    aerogpu = @{
        detected = $false
        pnp_device_id = $null
        adapter_registry_key = $null
        non_local_memory_size_mb = $null
        non_local_memory_size_mb_note = $null
        non_local_memory_size_mb_registry_path = $null
    }
}

function Add-Check([string]$key, [string]$title, [string]$status, [string]$summary, $data, [string[]]$details) {
    $report.checks[$key] = @{
        key = $key
        title = $title
        status = $status
        summary = $summary
        data = $data
        details = $details
    }
    $report.overall.status = Merge-Status $report.overall.status $status
}

# Ensure output directory exists early so we can always write the report.
try {
    if (-not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    }
} catch {
    $report.overall.status = "FAIL"
    $report.errors += ("Failed to create output directory '" + $outDir + "': " + $_.Exception.Message)
    # Can't guarantee report files can be written; still try.
}

if (-not $isAdmin) {
    $report.warnings += "Not running as Administrator; some checks may be incomplete (bcdedit/service queries) and C:\AeroGuestTools may not be writable."
}

# --- OS check ---
try {
    $os = Try-GetWmi "Win32_OperatingSystem" ""
    $osInfo = $null
    if ($os) {
        $osInfo = @{
            caption = $os.Caption
            version = $os.Version
            build_number = $os.BuildNumber
            service_pack_major = $os.ServicePackMajorVersion
            service_pack_minor = $os.ServicePackMinorVersion
            architecture = $os.OSArchitecture
        }
    }
    $osStatus = "PASS"
    $osSummary = ""
    $osDetails = @()
    if (-not $osInfo) {
        $osStatus = "WARN"
        $osSummary = "Unable to query Win32_OperatingSystem."
    } else {
        $osSummary = $osInfo.caption + " (version " + $osInfo.version + ", build " + $osInfo.build_number + ", SP" + $osInfo.service_pack_major + ", " + $osInfo.architecture + ")"
        if (-not ($osInfo.version -like "6.1*") -or ($osInfo.service_pack_major -ne 1)) {
            $osStatus = "WARN"
            $osDetails += "This tool targets Windows 7 SP1; results may be incomplete on other versions."
        }
    }
    Add-Check "os" "OS + Architecture" $osStatus $osSummary $osInfo $osDetails
} catch {
    Add-Check "os" "OS + Architecture" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

$gtSigningPolicy = $null
$gtCertsRequired = $null

# --- Guest Tools manifest (media integrity + provenance) ---
try {
    $manifestPath = Join-Path $scriptDir "manifest.json"
    $mStatus = "PASS"
    $mSummary = ""
    $mDetails = @()

    $mediaIntegrity = @{
        manifest_present = $false
        manifest_path = $manifestPath
        parse_ok = $false
        schema_version = $null
        package = $null
        signing_policy = $null
        certs_required = $null
        provenance_present = $false
        provenance = $null
        files_listed = 0
        manifest_includes_tools = $false
        tools_files_listed = 0
        files_checked = 0
        missing_files = @()
        size_mismatch_files = @()
        hash_mismatch_files = @()
        unreadable_files = @()
        file_results = @()
        disk_files_scanned = 0
        extra_files_not_in_manifest = @()
    }

    if (-not (Test-Path $manifestPath)) {
        $mStatus = "WARN"
        $mSummary = "manifest.json not found next to verify.ps1; cannot verify Guest Tools media integrity."
        $mDetails += "Tip: If you obtained the media as a .zip/.iso, ensure the full directory contents were copied intact."
    } else {
        $mediaIntegrity.manifest_present = $true
        $raw = Get-Content -Path $manifestPath -ErrorAction Stop | Out-String
        $parsed = Parse-JsonCompat $raw
        if (-not $parsed) {
            $mStatus = "FAIL"
            $mSummary = "manifest.json exists but could not be parsed. Media may be corrupted."
            $mDetails += "Remediation: Replace the Guest Tools ISO/zip with a fresh copy."
        } else {
            $mediaIntegrity.parse_ok = $true

            $schema = $null
            if ($parsed.ContainsKey("schema_version")) { $schema = $parsed["schema_version"] }
            $mediaIntegrity.schema_version = $schema
            $unsupportedSchema = $false
            $schemaStr = $null
            if ($schema -ne $null) { $schemaStr = ("" + $schema).Trim() }
            # Known manifest schema versions: 1/2/3/4. Warn (but do not fail) if we see an unknown schema.
            if ($schemaStr -and -not (@("1","2","3","4") -contains $schemaStr)) {
                $unsupportedSchema = $true
                $mDetails += ("WARN: Unknown manifest schema_version=" + $schemaStr + " (known: 1/2/3/4). verify.ps1 may be out of date.")
            }

            $pkg = $null
            $version = $null
            $buildId = $null
            $sde = $null
            if ($parsed.ContainsKey("package")) {
                $pkg = $parsed["package"]
                if ($pkg -and $pkg.ContainsKey("version")) { $version = $pkg["version"] }
                if ($pkg -and $pkg.ContainsKey("build_id")) { $buildId = $pkg["build_id"] }
                if ($pkg -and $pkg.ContainsKey("source_date_epoch")) { $sde = $pkg["source_date_epoch"] }
            } else {
                # Back-compat: older manifests may have these at the root.
                if ($parsed.ContainsKey("version")) { $version = $parsed["version"] }
                if ($parsed.ContainsKey("build_id")) { $buildId = $parsed["build_id"] }
                if ($parsed.ContainsKey("source_date_epoch")) { $sde = $parsed["source_date_epoch"] }
            }

            $mediaIntegrity.package = @{
                name = (if ($pkg -and $pkg.ContainsKey("name")) { $pkg["name"] } else { $null })
                version = $version
                build_id = $buildId
                source_date_epoch = $sde
            }

            $signingPolicy = $null
            $certsRequired = $null
            if ($parsed.ContainsKey("signing_policy")) { $signingPolicy = "" + $parsed["signing_policy"] }
            if ($parsed.ContainsKey("certs_required")) { $certsRequired = $parsed["certs_required"] }

            # Normalize legacy signing_policy values to the current surface (test|production|none).
            if ($signingPolicy) {
                $sp = $signingPolicy.ToLower()
                if (($sp -eq "testsigning") -or ($sp -eq "test-signing")) { $signingPolicy = "test" }
                if ($sp -eq "nointegritychecks") { $signingPolicy = "none" }
                if (($sp -eq "prod") -or ($sp -eq "whql")) { $signingPolicy = "production" }
            }

            if (($certsRequired -eq $null) -and $signingPolicy) {
                $sp = $signingPolicy.ToLower()
                if ($sp -eq "test") { $certsRequired = $true } else { $certsRequired = $false }
            }

            $mediaIntegrity.signing_policy = $signingPolicy
            $mediaIntegrity.certs_required = $certsRequired
            $gtSigningPolicy = $signingPolicy
            $gtCertsRequired = $certsRequired

            # Optional: packaging input provenance (added in manifest schema v3).
            # Keep backward-compatible: older manifests may not contain an `inputs` object.
            $inputs = $null
            if ($parsed.ContainsKey("inputs")) { $inputs = $parsed["inputs"] }
            if ($inputs -and ($inputs -is [System.Collections.IDictionary])) {
                $inputsData = @{
                    aero_packager_version = $null
                    packaging_spec = $null
                    windows_device_contract = $null
                }
                $inputsDetails = @()
                $inputsStatus = "PASS"
                $inputsSummary = "Packager input provenance recorded in manifest.json."

                if ($inputs.ContainsKey("aero_packager_version")) {
                    $inputsData.aero_packager_version = "" + $inputs["aero_packager_version"]
                    if ($inputsData.aero_packager_version) {
                        $inputsDetails += ("aero_packager_version: " + $inputsData.aero_packager_version)
                    }
                }

                $specIn = $null
                if ($inputs.ContainsKey("packaging_spec")) { $specIn = $inputs["packaging_spec"] }
                if ($specIn -and ($specIn -is [System.Collections.IDictionary])) {
                    $specPath = $null
                    $specSha = $null
                    if ($specIn.ContainsKey("path")) { $specPath = "" + $specIn["path"] }
                    if ($specIn.ContainsKey("sha256")) { $specSha = "" + $specIn["sha256"] }
                    $inputsData.packaging_spec = @{ path = $specPath; sha256 = $specSha }
                    if ($specPath -or $specSha) {
                        $inputsDetails += ("packaging_spec: " + $specPath + " sha256=" + $specSha)
                    }
                }

                $contractIn = $null
                if ($inputs.ContainsKey("windows_device_contract")) { $contractIn = $inputs["windows_device_contract"] }
                if ($contractIn -and ($contractIn -is [System.Collections.IDictionary])) {
                    $cPath = $null
                    $cSha = $null
                    $cName = $null
                    $cVer = $null
                    $cSchema = $null
                    if ($contractIn.ContainsKey("path")) { $cPath = "" + $contractIn["path"] }
                    if ($contractIn.ContainsKey("sha256")) { $cSha = "" + $contractIn["sha256"] }
                    if ($contractIn.ContainsKey("contract_name")) { $cName = "" + $contractIn["contract_name"] }
                    if ($contractIn.ContainsKey("contract_version")) { $cVer = "" + $contractIn["contract_version"] }
                    if ($contractIn.ContainsKey("schema_version")) { $cSchema = $contractIn["schema_version"] }
                    $inputsData.windows_device_contract = @{
                        path = $cPath
                        sha256 = $cSha
                        contract_name = $cName
                        contract_version = $cVer
                        schema_version = $cSchema
                    }
                    if ($cPath -or $cSha) {
                        $inputsDetails += ("windows_device_contract: " + $cPath + " sha256=" + $cSha)
                    }
                    if ($cName -or $cVer -or ($cSchema -ne $null)) {
                        $inputsDetails += ("contract: " + $cName + " v" + $cVer + " (schema_version=" + $cSchema + ")")
                    }
                }

                Add-Check "guest_tools_manifest_inputs" "Guest Tools Packaging Inputs (manifest.json)" $inputsStatus $inputsSummary $inputsData $inputsDetails
            }

            # Optional: manifest provenance (packaging spec + device contract hashes).
            $prov = $null
            if ($parsed.ContainsKey("provenance")) { $prov = $parsed["provenance"] }
            if ($prov -and ($prov -is [System.Collections.IDictionary])) {
                $psPath = $null
                $psSha = $null
                $cPath = $null
                $cSha = $null
                if ($prov.ContainsKey("packaging_spec_path")) { $psPath = "" + $prov["packaging_spec_path"] }
                if ($prov.ContainsKey("packaging_spec_sha256")) { $psSha = "" + $prov["packaging_spec_sha256"] }
                if ($prov.ContainsKey("windows_device_contract_path")) { $cPath = "" + $prov["windows_device_contract_path"] }
                if ($prov.ContainsKey("windows_device_contract_sha256")) { $cSha = "" + $prov["windows_device_contract_sha256"] }
                $mediaIntegrity.provenance_present = $true
                $mediaIntegrity.provenance = @{
                    packaging_spec_path = $psPath
                    packaging_spec_sha256 = $psSha
                    windows_device_contract_path = $cPath
                    windows_device_contract_sha256 = $cSha
                }
            } else {
                $mediaIntegrity.provenance_present = $false
                $mediaIntegrity.provenance = $null
            }

            $files = $null
            if ($parsed.ContainsKey("files")) { $files = $parsed["files"] }

            if (-not $files) {
                $mStatus = "WARN"
                $mSummary = "manifest.json parsed but does not contain a 'files' list; cannot verify media integrity."
            } else {
                $mediaIntegrity.files_listed = $files.Count

                foreach ($f in $files) {
                    $rel = $null
                    $expectedSha = $null
                    $expectedSize = $null

                    if ($f -and $f.ContainsKey("path")) { $rel = "" + $f["path"] }
                    if ($f -and $f.ContainsKey("sha256")) { $expectedSha = "" + $f["sha256"] }
                    if ($f -and $f.ContainsKey("size")) { $expectedSize = $f["size"] }

                    if (-not $rel -or $rel.Length -eq 0) { continue }

                    # Track whether the manifest explicitly lists tools\ payload files.
                    # (Some packaging modes may omit tools\ from the signed manifest.)
                    $relNorm = $rel.Replace("\", "/")
                    $relLower = $relNorm.ToLower()
                    if ($relLower.StartsWith("tools/")) {
                        $mediaIntegrity.tools_files_listed++
                        $mediaIntegrity.manifest_includes_tools = $true
                    }

                    $relFs = $rel.Replace("/", "\")
                    $full = Join-Path $scriptDir $relFs
                    $exists = Test-Path $full

                    $actualSha = $null
                    $actualSize = $null
                    $status = "PASS"

                    if (-not $exists) {
                        $status = "FAIL"
                        $mediaIntegrity.missing_files += $rel
                    } else {
                        try {
                            $item = Get-Item -LiteralPath $full -ErrorAction Stop
                            $actualSize = $item.Length
                        } catch {
                            $actualSize = $null
                        }

                        $actualSha = Get-FileSha256Hex $full
                        if (($expectedSize -ne $null) -and ($actualSize -ne $null)) {
                            $expSize = $null
                            try { $expSize = [Int64]$expectedSize } catch { $expSize = $null }
                            if (($expSize -ne $null) -and ($expSize -ne [Int64]$actualSize)) {
                                $status = "FAIL"
                                $mediaIntegrity.size_mismatch_files += $rel
                            }
                        }

                        if (-not $actualSha) {
                            if ($status -eq "PASS") { $status = "WARN" }
                            $mediaIntegrity.unreadable_files += $rel
                        } elseif ($expectedSha -and ($actualSha.ToLower() -ne $expectedSha.ToLower())) {
                            $status = "FAIL"
                            $mediaIntegrity.hash_mismatch_files += $rel
                        }
                    }

                    $mediaIntegrity.file_results += @{
                        path = $rel
                        exists = $exists
                        expected_sha256 = $expectedSha
                        actual_sha256 = $actualSha
                        expected_size = $expectedSize
                        actual_size = $actualSize
                        status = $status
                    }
                }

                $mediaIntegrity.files_checked = $mediaIntegrity.file_results.Count
                $missingCount = $mediaIntegrity.missing_files.Count
                $sizeMismatchCount = $mediaIntegrity.size_mismatch_files.Count
                $hashMismatchCount = $mediaIntegrity.hash_mismatch_files.Count
                $unreadableCount = $mediaIntegrity.unreadable_files.Count

                if ($missingCount -gt 0 -or $hashMismatchCount -gt 0 -or $sizeMismatchCount -gt 0) {
                    $mStatus = "FAIL"
                } elseif ($unreadableCount -gt 0) {
                    $mStatus = "WARN"
                } else {
                    $mStatus = "PASS"
                }

                if ($unsupportedSchema) { $mStatus = Merge-Status $mStatus "WARN" }

                $mSummary = "Guest Tools media: version=" + $version + ", build_id=" + $buildId + "; files checked=" + $mediaIntegrity.files_checked + " (missing=" + $missingCount + ", size_mismatch=" + $sizeMismatchCount + ", hash_mismatch=" + $hashMismatchCount + ", unreadable=" + $unreadableCount + ")"
                if ($unsupportedSchema) { $mSummary += "; schema_version=" + $schemaStr + " (unknown)" }
                if ($gtSigningPolicy) {
                    $mSummary += "; signing_policy=" + $gtSigningPolicy
                    if ($gtCertsRequired -ne $null) { $mSummary += ", certs_required=" + $gtCertsRequired }
                }
                if ($mediaIntegrity.tools_files_listed -gt 0) {
                    $mSummary += "; tools_files_listed=" + $mediaIntegrity.tools_files_listed
                }

                if ($missingCount -gt 0) {
                    foreach ($p in $mediaIntegrity.missing_files) { $mDetails += ("FAIL: Missing file: " + $p) }
                }
                if ($sizeMismatchCount -gt 0) {
                    foreach ($r in $mediaIntegrity.file_results) {
                        if (($r.exists -eq $true) -and ($r.expected_size -ne $null) -and ($r.actual_size -ne $null)) {
                            $exp = $null
                            $act = $null
                            try { $exp = [Int64]$r.expected_size } catch { $exp = $null }
                            try { $act = [Int64]$r.actual_size } catch { $act = $null }
                            if (($exp -ne $null) -and ($act -ne $null) -and ($exp -ne $act)) {
                                $mDetails += ("FAIL: Size mismatch: " + $r.path + " (expected " + $exp + " bytes, got " + $act + " bytes)")
                            }
                        }
                    }
                }
                if ($hashMismatchCount -gt 0) {
                    foreach ($r in $mediaIntegrity.file_results) {
                        if (($r.status -eq "FAIL") -and ($r.exists -eq $true) -and $r.expected_sha256 -and $r.actual_sha256) {
                            $exp = "" + $r.expected_sha256
                            $act = "" + $r.actual_sha256
                            $expShort = $exp
                            $actShort = $act
                            if ($expShort.Length -gt 12) { $expShort = $expShort.Substring(0, 12) }
                            if ($actShort.Length -gt 12) { $actShort = $actShort.Substring(0, 12) }
                            $mDetails += ("FAIL: SHA-256 mismatch: " + $r.path + " (expected " + $expShort + "... got " + $actShort + "...)")
                        }
                    }
                }
                if ($unreadableCount -gt 0) {
                    foreach ($p in $mediaIntegrity.unreadable_files) { $mDetails += ("WARN: Unable to hash file (read error): " + $p) }
                }

                if (($missingCount -gt 0) -or ($sizeMismatchCount -gt 0) -or ($hashMismatchCount -gt 0) -or ($unreadableCount -gt 0)) {
                    $mDetails += "Remediation: Replace the Guest Tools ISO/zip with a fresh copy (do not mix driver folders across versions)."
                    $mDetails += "See: docs/windows7-driver-troubleshooting.md#issue-guest-tools-media-integrity-check-fails-manifest-hash-mismatch"
                }

                # Mixed-media advisory: detect extra files that exist on disk but are not listed
                # in manifest.json. These often happen when users merge folders from different
                # Guest Tools versions, which can lead to subtle driver binding issues.
                try {
                    $expected = @{}
                    foreach ($r in $mediaIntegrity.file_results) {
                        if (-not $r -or -not $r.path) { continue }
                        $p = ("" + $r.path).Trim()
                        if ($p.Length -eq 0) { continue }
                        $p = $p.Replace("\", "/")
                        if ($p.StartsWith("./")) { $p = $p.Substring(2) }
                        while ($p.StartsWith("/")) { $p = $p.Substring(1) }
                        if ($p.Length -eq 0) { continue }
                        $pl = $p.ToLower()
                        $expected[$pl] = $true
                    }
                    # manifest.json itself is intentionally treated as expected even if the
                    # manifest's files[] list does not include it.
                    $expected["manifest.json"] = $true

                    $rootItem = Get-Item -LiteralPath $scriptDir -ErrorAction Stop
                    $rootFull = "" + $rootItem.FullName
                    $prefix = $rootFull
                    if (-not ($prefix.EndsWith("\") -or $prefix.EndsWith("/"))) { $prefix += "\" }
                    $prefixLower = $prefix.ToLower()

                    # Exclude the report output directory (C:\AeroGuestTools\) from the scan when
                    # the Guest Tools root is located on the system drive (e.g. extracted/copy-run).
                    $outDirFull = $null
                    try { $outDirFull = [System.IO.Path]::GetFullPath($outDir) } catch { $outDirFull = $outDir }
                    $outDirFullLower = ""
                    try { $outDirFullLower = ("" + $outDirFull).ToLower() } catch { $outDirFullLower = "" }
                    $outDirPrefixLower = $outDirFullLower
                    if ($outDirPrefixLower -and (-not ($outDirPrefixLower.EndsWith("\") -or $outDirPrefixLower.EndsWith("/")))) { $outDirPrefixLower += "\" }

                    $outDirEqualsRoot = $false
                    $outDirWithinRoot = $false
                    try {
                        if ($outDirFullLower -and $rootFull -and ($outDirFullLower -eq ("" + $rootFull).ToLower())) {
                            $outDirEqualsRoot = $true
                            $outDirWithinRoot = $true
                        } elseif ($outDirFullLower -and $outDirFullLower.StartsWith($prefixLower)) {
                            $outDirWithinRoot = $true
                        }
                    } catch { }

                    # If the Guest Tools root equals the output dir, exclude only known output artifacts.
                    $skipOutputRelWhenRoot = @(
                        "report.json",
                        "report.txt",
                        "dbgctl_status.txt",
                        "dbgctl_version.txt",
                        "install.log",
                        "installed-driver-packages.txt",
                        "installed-certs.txt",
                        "installed-media.txt",
                        "testsigning.enabled-by-aero.txt",
                        "nointegritychecks.enabled-by-aero.txt",
                        "storage-preseed.skipped.txt"
                    )

                    $diskFiles = @()
                    foreach ($it in (Get-ChildItem -LiteralPath $rootFull -Recurse -Force -ErrorAction Stop)) {
                        if ($it.PSIsContainer) { continue }
                        $full = "" + $it.FullName
                        if ($outDirWithinRoot -and (-not $outDirEqualsRoot) -and $outDirPrefixLower) {
                            try {
                                if ($full -and ($full.ToLower().StartsWith($outDirPrefixLower))) { continue }
                            } catch { }
                        }
                        $rel = $null
                        if ($full -and ($full.ToLower().StartsWith($prefixLower))) {
                            $rel = $full.Substring($prefix.Length)
                        } else {
                            $rel = "" + $it.Name
                        }
                        $rel = $rel.Replace("\", "/")
                        if ($rel.StartsWith("./")) { $rel = $rel.Substring(2) }
                        while ($rel.StartsWith("/")) { $rel = $rel.Substring(1) }
                        if (-not $rel -or $rel.Length -eq 0) { continue }
                        if ($outDirEqualsRoot) {
                            # When output dir == root, skip only known artifacts instead of skipping the whole scan.
                            if ($skipOutputRelWhenRoot -contains $rel.ToLower()) { continue }
                        }
                        $diskFiles += $rel
                    }

                    $extra = @()
                    foreach ($p in $diskFiles) {
                        $k = ("" + $p).ToLower()
                        if (-not $expected.ContainsKey($k)) { $extra += $p }
                    }
                    $extra = @($extra | Sort-Object)

                    $mediaIntegrity.disk_files_scanned = $diskFiles.Count
                    $mediaIntegrity.extra_files_not_in_manifest = $extra

                    $extraStatus = "PASS"
                    if ($extra.Count -gt 0) { $extraStatus = "WARN" }

                    $extraSummary = "Extra files not in manifest: " + $extra.Count + " (disk files scanned: " + $diskFiles.Count + ")"
                    $extraDetails = @()
                    if ($extra.Count -gt 0) {
                        $extraDetails += ($extra.Count.ToString() + " extra file(s) exist on disk but are not listed in manifest.json. This can indicate mixed media (merged Guest Tools versions), which can cause subtle driver binding issues.")
                        $maxList = 25
                        $shown = 0
                        foreach ($p in $extra) {
                            if ($shown -ge $maxList) { break }
                            $extraDetails += ("Extra: " + $p)
                            $shown++
                        }
                        if ($extra.Count -gt $maxList) {
                            $extraDetails += ("... and " + ($extra.Count - $maxList).ToString() + " more (see report.json for the full list).")
                        }
                        $extraDetails += "Remediation: Replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions."
                    } else {
                        $extraDetails += "No extra files found under Guest Tools root."
                    }

                    $extraData = @{
                        guest_tools_root = $rootFull
                        manifest_path = $manifestPath
                        expected_paths_count = $expected.Count
                        disk_files_scanned = $diskFiles.Count
                        extra_files = $extra
                    }

                    Add-Check "extra_files_not_in_manifest" "Extra Files Not In Manifest (mixed media check)" $extraStatus $extraSummary $extraData $extraDetails
                } catch {
                    $mediaIntegrity.extra_files_not_in_manifest_error = $_.Exception.Message
                    Add-Check "extra_files_not_in_manifest" "Extra Files Not In Manifest (mixed media check)" "WARN" ("Failed: " + $_.Exception.Message) $null @("Remediation: Replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.")
                }
            }

            # Provenance is optional for back-compat with older media, but surface it when present.
            if (-not $mediaIntegrity.provenance_present) {
                $mStatus = Merge-Status $mStatus "WARN"
                $mDetails += "WARN: manifest.json does not contain provenance fields (packaging spec + device contract hashes). Media may have been built with an older packager."
            } else {
                $prov = $mediaIntegrity.provenance
                $missing = @()
                foreach ($k in @("packaging_spec_path","packaging_spec_sha256","windows_device_contract_path","windows_device_contract_sha256")) {
                    if (-not $prov.ContainsKey($k) -or -not $prov[$k] -or (("" + $prov[$k]).Trim().Length -eq 0)) { $missing += $k }
                }
                if ($missing.Count -gt 0) {
                    $mStatus = Merge-Status $mStatus "WARN"
                    $mDetails += ("WARN: manifest.json provenance is missing field(s): " + ($missing -join ", "))
                }
                $mDetails += ("Provenance: packaging_spec_path=" + ("" + $prov.packaging_spec_path))
                $mDetails += ("Provenance: packaging_spec_sha256=" + ("" + $prov.packaging_spec_sha256))
                $mDetails += ("Provenance: windows_device_contract_path=" + ("" + $prov.windows_device_contract_path))
                $mDetails += ("Provenance: windows_device_contract_sha256=" + ("" + $prov.windows_device_contract_sha256))
            }
        }
    }

    $report.media_integrity = $mediaIntegrity
    Add-Check "guest_tools_manifest" "Guest Tools Media Integrity (manifest.json)" $mStatus $mSummary $mediaIntegrity $mDetails
} catch {
    $report.media_integrity = @{
        manifest_path = (Join-Path $scriptDir "manifest.json")
        error = $_.Exception.Message
    }
    Add-Check "guest_tools_manifest" "Guest Tools Media Integrity (manifest.json)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Guest Tools media sanity: cert files vs signing_policy ---
try {
    $certDir = Join-Path $scriptDir "certs"
    $certFiles = @()
    if (Test-Path $certDir) {
        $certFiles += (Get-ChildItem -Path $certDir -Recurse -Filter *.cer -ErrorAction SilentlyContinue)
        $certFiles += (Get-ChildItem -Path $certDir -Recurse -Filter *.crt -ErrorAction SilentlyContinue)
        $certFiles += (Get-ChildItem -Path $certDir -Recurse -Filter *.p7b -ErrorAction SilentlyContinue)
    }

    $policy = $gtSigningPolicy
    if ((-not $policy) -and $report.media_integrity -and ($report.media_integrity -is [hashtable]) -and $report.media_integrity.signing_policy) {
        $policy = "" + $report.media_integrity.signing_policy
    }
    $policyLower = ""
    if ($policy) { $policyLower = ("" + $policy).ToLower() }

    $st = "PASS"
    $sum = ""
    $det = @()

    if (-not $policyLower -or $policyLower.Length -eq 0) {
        $sum = "signing_policy unknown; certificate files under certs\\: " + $certFiles.Count
        if ($certFiles -and $certFiles.Count -gt 0) {
            $st = "WARN"
            $det += "WARN: Certificate file(s) exist under certs\\, but signing_policy is missing/unknown."
        }
    } else {
        $knownPolicy = (@("test","production","none") -contains $policyLower)
        if (-not $knownPolicy) {
            $st = "WARN"
            $det += ("WARN: Unknown signing_policy='" + $policyLower + "'. Expected: test|production|none.")
        }

        if ($policyLower -ne "test") {
            # For production/none (and any unknown policy), cert payloads are suspicious.
            if ($certFiles -and $certFiles.Count -gt 0) {
                $st = Merge-Status $st "WARN"
                $sum = "signing_policy=" + $policyLower + " but found " + $certFiles.Count + " certificate file(s) under certs\\."
                $names = @($certFiles | ForEach-Object { $_.Name })
                if ($names.Count -le 10) {
                    $det += ("Cert files: " + ($names -join ", "))
                } else {
                    $preview = @($names | Select-Object -First 10)
                    $det += ("Cert files: " + ($preview -join ", ") + " ... (" + $names.Count + " total)")
                }
                $det += ("Remediation: Rebuild/replace the Guest Tools media so certs\\ is empty/absent for signing_policy=" + $policyLower + ".")
                $det += "If you intended to use test-signed drivers, set signing_policy=test and include only the required signing certificate(s) under certs\\."
            } else {
                $sum = "signing_policy=" + $policyLower + "; no certificate files found under certs\\ (expected)."
            }
        } else {
            # signing_policy=test: certificate files are expected; do not warn.
            $sum = "signing_policy=" + $policyLower + "; certificate files under certs\\: " + $certFiles.Count
        }
    }

    $data = @{
        signing_policy = $policy
        cert_dir = $certDir
        cert_dir_exists = (Test-Path $certDir)
        cert_files = @($certFiles | ForEach-Object { $_.FullName })
    }
    Add-Check "certs_on_media_policy_mismatch" "Guest Tools Media Sanity (certs vs signing_policy)" $st $sum $data $det
} catch {
    Add-Check "certs_on_media_policy_mismatch" "Guest Tools Media Sanity (certs vs signing_policy)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Guest Tools setup state (C:\AeroGuestTools\*) ---
$gtInstalledDriverPackages = @()
try {
    $installLog = Join-Path $outDir "install.log"
    $pkgList = Join-Path $outDir "installed-driver-packages.txt"
    $certList = Join-Path $outDir "installed-certs.txt"
    $installedMediaPath = Join-Path $outDir "installed-media.txt"
    $stateTestSign = Join-Path $outDir "testsigning.enabled-by-aero.txt"
    $stateNoIntegrity = Join-Path $outDir "nointegritychecks.enabled-by-aero.txt"

    if (Test-Path $pkgList) {
        foreach ($line in (Get-Content -Path $pkgList -ErrorAction SilentlyContinue)) {
            $t = ("" + $line).Trim()
            if ($t.Length -eq 0) { continue }
            $gtInstalledDriverPackages += $t
        }
    }

    $installedCertThumbprints = @()
    if (Test-Path $certList) {
        foreach ($line in (Get-Content -Path $certList -ErrorAction SilentlyContinue)) {
            $t = ("" + $line).Trim()
            if ($t.Length -eq 0) { continue }
            $installedCertThumbprints += $t
        }
    }

    $installedMediaVars = @{}
    $installedMediaRaw = $null
    if (Test-Path $installedMediaPath) {
        $lines = @(Get-Content -Path $installedMediaPath -ErrorAction SilentlyContinue)
        $installedMediaRaw = ($lines -join "`r`n")
        foreach ($line in $lines) {
            $t = ("" + $line).Trim()
            if ($t.Length -eq 0) { continue }
            if ($t -match '^([^=]+)=(.*)$') {
                $installedMediaVars[$matches[1]] = $matches[2]
            }
        }
    }
    $installedMediaGtVersion = $null
    $installedMediaGtBuildId = $null
    if ($installedMediaVars.ContainsKey("GT_VERSION")) {
        $installedMediaGtVersion = ("" + $installedMediaVars["GT_VERSION"]).Trim()
        if ($installedMediaGtVersion.Length -eq 0) { $installedMediaGtVersion = $null }
    }
    if ($installedMediaVars.ContainsKey("GT_BUILD_ID")) {
        $installedMediaGtBuildId = ("" + $installedMediaVars["GT_BUILD_ID"]).Trim()
        if ($installedMediaGtBuildId.Length -eq 0) { $installedMediaGtBuildId = $null }
    }

    $st = "PASS"
    $sum = ""
    $det = @()

    $hasAny = (Test-Path $installLog) -or (Test-Path $pkgList) -or (Test-Path $certList) -or (Test-Path $installedMediaPath) -or (Test-Path $storagePreseedSkipMarker)
    if (-not $hasAny) {
        $st = "WARN"
        $sum = "No Guest Tools setup state files found under " + $outDir + " (setup.cmd may not have been run yet)."
    } else {
        $sum = "install.log=" + (Test-Path $installLog) + ", installed-driver-packages=" + $gtInstalledDriverPackages.Count + ", installed-certs=" + $installedCertThumbprints.Count + ", installed-media=" + (Test-Path $installedMediaPath)
        if (Test-Path $stateTestSign) { $det += "TestSigning was enabled by setup.cmd (marker file present)." }
        if (Test-Path $stateNoIntegrity) { $det += "nointegritychecks was enabled by setup.cmd (marker file present)." }
        if (Test-Path $storagePreseedSkipMarker) { $det += "Storage pre-seeding was skipped by setup.cmd (/skipstorage) (marker file present)." }

        if (Test-Path $installedMediaPath) {
            $det += ("installed-media.txt: GT_VERSION=" + $installedMediaGtVersion + ", GT_BUILD_ID=" + $installedMediaGtBuildId)
            if ($installedMediaVars.ContainsKey("manifest_path")) {
                $det += ("installed-media.txt: manifest_path=" + ("" + $installedMediaVars["manifest_path"]))
            }

            $currentVersion = $null
            $currentBuildId = $null
            if ($report.media_integrity -and ($report.media_integrity -is [hashtable])) {
                $pkg = $report.media_integrity.package
                if ($pkg -and ($pkg -is [hashtable])) {
                    if ($pkg.ContainsKey("version")) { $currentVersion = $pkg["version"] }
                    if ($pkg.ContainsKey("build_id")) { $currentBuildId = $pkg["build_id"] }
                }
            }
            if ($currentVersion) { $currentVersion = ("" + $currentVersion).Trim() }
            if ($currentBuildId) { $currentBuildId = ("" + $currentBuildId).Trim() }

            $mismatch = $false
            if ($installedMediaGtVersion -and $currentVersion) {
                if ($installedMediaGtVersion.ToLower() -ne $currentVersion.ToLower()) { $mismatch = $true }
            }
            if ($installedMediaGtBuildId -and $currentBuildId) {
                if ($installedMediaGtBuildId.ToLower() -ne $currentBuildId.ToLower()) { $mismatch = $true }
            }

            if ($mismatch) {
                $st = Merge-Status $st "WARN"
                $sum += "; WARN: installed media differs from current media"
                $det += ("WARN: setup.cmd was run from Guest Tools media version=" + $installedMediaGtVersion + ", build_id=" + $installedMediaGtBuildId + ", but the current media manifest.json is version=" + $currentVersion + ", build_id=" + $currentBuildId + ".")
                $det += "Remediation: re-run setup.cmd from the current ISO."
            }
        }
    }

    $data = @{
        install_root = $outDir
        install_log_path = $installLog
        install_log_exists = (Test-Path $installLog)
        installed_driver_packages_path = $pkgList
        installed_driver_packages = $gtInstalledDriverPackages
        installed_certs_path = $certList
        installed_certs = $installedCertThumbprints
        testsigning_marker_path = $stateTestSign
        testsigning_marker_exists = (Test-Path $stateTestSign)
        nointegritychecks_marker_path = $stateNoIntegrity
        nointegritychecks_marker_exists = (Test-Path $stateNoIntegrity)
        installed_media_path = $installedMediaPath
        installed_media_exists = (Test-Path $installedMediaPath)
        installed_media_raw = $installedMediaRaw
        installed_media_vars = $installedMediaVars
        storage_preseed_skipped_marker_path = $storagePreseedSkipMarker
        storage_preseed_skipped_marker_exists = (Test-Path $storagePreseedSkipMarker)
    }

    Add-Check "guest_tools_setup_state" "Guest Tools Setup State" $st $sum $data $det
} catch {
    Add-Check "guest_tools_setup_state" "Guest Tools Setup State" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Guest Tools config (config\devices.cmd) ---
try {
    $cfgStatus = "PASS"
    $cfgSummary = ""
    $cfgDetails = @()

    if (-not $gtConfig.found) {
        $cfgStatus = "WARN"
        $cfgSummary = "config\\devices.cmd not found; some storage/device binding checks will be heuristic-only."
    } elseif ($gtConfig.exit_code -ne 0) {
        $cfgStatus = "WARN"
        $cfgSummary = "config\\devices.cmd exists but could not be loaded via cmd.exe (exit code " + $gtConfig.exit_code + ")."
    } else {
        $cfgSummary = "Loaded config\\devices.cmd (AERO_* variables: " + $gtConfig.vars.Count + ")"
        if (-not $cfgVirtioBlkService) {
            $cfgStatus = "WARN"
            $cfgDetails += "AERO_VIRTIO_BLK_SERVICE is not set."
        }
        if (-not $cfgVirtioBlkHwids -or $cfgVirtioBlkHwids.Count -eq 0) {
            $cfgStatus = "WARN"
            $cfgDetails += "AERO_VIRTIO_BLK_HWIDS is not set."
        } else {
             $expected = "PCI\VEN_1AF4&DEV_1042&REV_01"
             $found = $false
             $hasModern = $false
             $hasTransitional = $false
             $transitionalDevId = "1001"
             foreach ($h in $cfgVirtioBlkHwids) {
                 if (-not $h) { continue }
                 $u = ("" + $h).ToUpper()
                 if ($u.Contains("DEV_1042")) { $hasModern = $true }
                 if ($u.Contains(("DEV_" + $transitionalDevId))) { $hasTransitional = $true }
                 if (("" + $h).ToLower() -eq $expected.ToLower()) { $found = $true }
             }
            if (-not $hasModern) {
                $cfgStatus = "WARN"
                $cfgDetails += "AERO_VIRTIO_BLK_HWIDS does not include DEV_1042 (virtio-blk modern ID)."
            }
             if ($hasTransitional) {
                 $cfgStatus = "WARN"
                 $cfgDetails += "AERO_VIRTIO_BLK_HWIDS contains a transitional virtio-blk ID (1AF4:1001). Aero Win7 contract v1 is modern-only (1AF4:1042)."
             }
             if (-not $found) {
                 $cfgStatus = "WARN"
                 $cfgDetails += ("AERO_VIRTIO_BLK_HWIDS should include '" + $expected + "' (Aero Win7 virtio contract v1 expects REV_01).")
            }
        }
        if (-not $cfgVirtioSndService) {
            $cfgStatus = "WARN"
            $cfgDetails += "AERO_VIRTIO_SND_SERVICE is not set (default for Aero in-tree drivers: aero_virtio_snd)."
        }
        if (-not $cfgVirtioSndHwids -or $cfgVirtioSndHwids.Count -eq 0) {
            $cfgStatus = "WARN"
            $cfgDetails += "AERO_VIRTIO_SND_HWIDS is not set."
        } else {
            $expected = "PCI\VEN_1AF4&DEV_1059&REV_01"
            $found = $false
            $hasTransitional = $false
            foreach ($h in $cfgVirtioSndHwids) {
                if (-not $h) { continue }
                $u = ("" + $h).ToUpper()
                if ($u.Contains("DEV_1018")) { $hasTransitional = $true }
                if (("" + $h).ToLower() -eq $expected.ToLower()) { $found = $true }
            }
            if (-not $found) {
                $cfgStatus = "WARN"
                $cfgDetails += ("AERO_VIRTIO_SND_HWIDS should include '" + $expected + "' (Aero Win7 virtio contract v1 expects REV_01).")
            }
            if ($hasTransitional) {
                $cfgStatus = "WARN"
                $cfgDetails += "AERO_VIRTIO_SND_HWIDS contains PCI\VEN_1AF4&DEV_1018 (virtio-snd transitional ID). Aero Win7 contract v1 is modern-only (PCI\VEN_1AF4&DEV_1059)."
            }
        }
        if ($cfgVirtioNetHwids -and $cfgVirtioNetHwids.Count -gt 0) {
             $expected = "PCI\VEN_1AF4&DEV_1041&REV_01"
             $found = $false
             $hasModern = $false
             $hasTransitional = $false
             $transitionalDevId = "1000"
             foreach ($h in $cfgVirtioNetHwids) {
                 if (-not $h) { continue }
                 $u = ("" + $h).ToUpper()
                 if ($u.Contains("DEV_1041")) { $hasModern = $true }
                 if ($u.Contains(("DEV_" + $transitionalDevId))) { $hasTransitional = $true }
                 if (("" + $h).ToLower() -eq $expected.ToLower()) { $found = $true }
             }
            if (-not $hasModern) {
                $cfgStatus = "WARN"
                $cfgDetails += "AERO_VIRTIO_NET_HWIDS does not include DEV_1041 (virtio-net modern ID)."
            }
             if ($hasTransitional) {
                 $cfgStatus = "WARN"
                 $cfgDetails += "AERO_VIRTIO_NET_HWIDS contains a transitional virtio-net ID (1AF4:1000). Aero Win7 contract v1 is modern-only (1AF4:1041)."
             }
             if (-not $found) {
                 $cfgStatus = "WARN"
                 $cfgDetails += ("AERO_VIRTIO_NET_HWIDS should include '" + $expected + "' (Aero Win7 virtio contract v1 expects REV_01).")
            }
        }
        if ($cfgVirtioInputHwids -and $cfgVirtioInputHwids.Count -gt 0) {
            $hasModern = $false
            $hasTransitional = $false
            $expected = "PCI\VEN_1AF4&DEV_1052&REV_01"
            $found = $false
            foreach ($h in $cfgVirtioInputHwids) {
                if (-not $h) { continue }
                $u = ("" + $h).ToUpper()
                if ($u.Contains("DEV_1052")) { $hasModern = $true }
                if ($u.Contains("DEV_1011")) { $hasTransitional = $true }
                if (("" + $h).ToLower() -eq $expected.ToLower()) { $found = $true }
            }
            if (-not $hasModern) {
                $cfgStatus = "WARN"
                $cfgDetails += "AERO_VIRTIO_INPUT_HWIDS does not include DEV_1052 (virtio-input modern ID)."
            }
            if ($hasTransitional) {
                $cfgStatus = "WARN"
                $cfgDetails += "AERO_VIRTIO_INPUT_HWIDS contains DEV_1011 (virtio-input transitional ID). Aero Win7 contract v1 is modern-only (DEV_1052)."
            }
            if (-not $found) {
                $cfgStatus = "WARN"
                $cfgDetails += ("AERO_VIRTIO_INPUT_HWIDS should include '" + $expected + "' (Aero Win7 virtio contract v1 expects REV_01).")
            }
        }
        if ($cfgVirtioBlkService) { $cfgDetails += "AERO_VIRTIO_BLK_SERVICE=" + $cfgVirtioBlkService }
        if ($cfgVirtioBlkSys) { $cfgDetails += "AERO_VIRTIO_BLK_SYS=" + $cfgVirtioBlkSys }
        if ($cfgVirtioSndService) { $cfgDetails += "AERO_VIRTIO_SND_SERVICE=" + $cfgVirtioSndService }
        if ($cfgVirtioSndSys) { $cfgDetails += "AERO_VIRTIO_SND_SYS=" + $cfgVirtioSndSys }
        if ($cfgVirtioBlkHwids -and $cfgVirtioBlkHwids.Count -gt 0) { $cfgDetails += "AERO_VIRTIO_BLK_HWIDS=" + ($cfgVirtioBlkHwids -join ", ") }
        if ($cfgVirtioNetHwids -and $cfgVirtioNetHwids.Count -gt 0) { $cfgDetails += "AERO_VIRTIO_NET_HWIDS=" + ($cfgVirtioNetHwids -join ", ") }
        if ($cfgVirtioSndHwids -and $cfgVirtioSndHwids.Count -gt 0) { $cfgDetails += "AERO_VIRTIO_SND_HWIDS=" + ($cfgVirtioSndHwids -join ", ") }
        if ($cfgVirtioInputHwids -and $cfgVirtioInputHwids.Count -gt 0) { $cfgDetails += "AERO_VIRTIO_INPUT_HWIDS=" + ($cfgVirtioInputHwids -join ", ") }
        if ($cfgGpuHwids -and $cfgGpuHwids.Count -gt 0) { $cfgDetails += "AERO_GPU_HWIDS=" + ($cfgGpuHwids -join ", ") }
    }

    $data = @{
        config_file = $gtConfig.file_path
        config_found = $gtConfig.found
        config_exit_code = $gtConfig.exit_code
        vars = $gtConfig.vars
    }
    Add-Check "guest_tools_config" "Guest Tools Config (devices.cmd)" $cfgStatus $cfgSummary $data $cfgDetails
} catch {
    Add-Check "guest_tools_config" "Guest Tools Config (devices.cmd)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Packaged drivers on the Guest Tools media (parse INFs for provenance/HWIDs) ---
$packagedDriversSummary = $null
try {
    $is64 = $false
    if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
        $is64 = ("" + $report.checks.os.data.architecture) -match '64'
    } else {
        $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
    }

    $arch = (if ($is64) { "amd64" } else { "x86" })
    $driversRoot = Join-Path (Join-Path $scriptDir "drivers") $arch

    $summary = @{
        guest_tools_root = $scriptDir
        arch = $arch
        drivers_root = $driversRoot
        drivers_root_exists = (Test-Path $driversRoot)
        driver_folders = @()
        total_driver_folders = 0
        total_inf_files = 0
        total_hwid_prefixes = 0
        total_add_services = 0
        inf_parse_failures = 0
    }

    $status = "PASS"
    $sumText = ""
    $details = @()

    if (-not (Test-Path $driversRoot)) {
        $status = "WARN"
        $sumText = "drivers\\" + $arch + " not found under the Guest Tools root; packaged driver inventory unavailable."
        $details += "If you are running verify.ps1 from a copied/extracted folder, ensure you copied the full Guest Tools media (including drivers/)."
    } else {
        $folders = Get-ChildItem -Path $driversRoot -ErrorAction Stop | Where-Object { $_.PSIsContainer }
        $summary.total_driver_folders = $folders.Count

        foreach ($folder in $folders) {
            $infFiles = Get-ChildItem -Path $folder.FullName -Recurse -Filter *.inf -ErrorAction SilentlyContinue | Where-Object { -not $_.PSIsContainer }
            $sysFiles = Get-ChildItem -Path $folder.FullName -Recurse -Filter *.sys -ErrorAction SilentlyContinue | Where-Object { -not $_.PSIsContainer }
            $catFiles = Get-ChildItem -Path $folder.FullName -Recurse -Filter *.cat -ErrorAction SilentlyContinue | Where-Object { -not $_.PSIsContainer }

            $infMeta = @()
            foreach ($inf in $infFiles) {
                $m = Parse-InfMetadata $inf.FullName

                # Store media-relative path for portability.
                $rel = $inf.FullName
                try {
                    if ($rel.ToLower().StartsWith($scriptDir.ToLower())) {
                        $rel = $rel.Substring($scriptDir.Length).TrimStart('\')
                        $rel = $rel.Replace('\', '/')
                    }
                } catch { }
                $m.inf_rel_path = $rel

                if ($m.parse_errors -and $m.parse_errors.Count -gt 0) { $summary.inf_parse_failures++ }
                $summary.total_inf_files++
                $summary.total_hwid_prefixes += $m.hwid_prefixes.Count
                $summary.total_add_services += $m.add_services.Count
                $infMeta += $m
            }

            $summary.driver_folders += @{
                name = "" + $folder.Name
                path = "" + $folder.FullName
                inf_count = $infFiles.Count
                sys_count = $sysFiles.Count
                cat_count = $catFiles.Count
                infs = $infMeta
            }
        }

        # Cross-check that config\devices.cmd aligns with packaged driver INFs (high-signal for mixed/incorrect media).
        $serviceSet = @{}
        $mediaHwids = @()
        foreach ($pkg in $summary.driver_folders) {
            if (-not $pkg.infs) { continue }
            foreach ($inf in $pkg.infs) {
                if ($inf.add_services) {
                    foreach ($svc in $inf.add_services) {
                        if ($svc) { $serviceSet[$svc.ToLower()] = $svc }
                    }
                }
                if ($inf.hwid_prefixes) {
                    foreach ($h in $inf.hwid_prefixes) {
                        if ($h) { $mediaHwids += $h }
                    }
                }
            }
        }
        $summary.media_add_services = @()
        foreach ($k in $serviceSet.Keys) { $summary.media_add_services += $serviceSet[$k] }

        if ($cfgVirtioBlkService -and (-not $serviceSet.ContainsKey($cfgVirtioBlkService.ToLower()))) {
            if ($storagePreseedSkipped) {
                $details += ("NOTE: storage pre-seeding was skipped by setup.cmd (/skipstorage). config\\devices.cmd AERO_VIRTIO_BLK_SERVICE='" + $cfgVirtioBlkService + "' does not match any AddService name found in packaged INFs (expected for partial driver payloads).")
            } else {
                $status = Merge-Status $status "WARN"
                $details += ("config\\devices.cmd AERO_VIRTIO_BLK_SERVICE='" + $cfgVirtioBlkService + "' does not match any AddService name found in packaged INFs. Boot-critical registry seeding may be wrong for this media.")
            }
        }
        if ($cfgVirtioBlkHwids -and $cfgVirtioBlkHwids.Count -gt 0 -and $mediaHwids.Count -gt 0) {
            $missingCfg = @()
            foreach ($h in $cfgVirtioBlkHwids) {
                $found = $false
                foreach ($mh in $mediaHwids) {
                    if ($mh.ToUpper().StartsWith($h.ToUpper())) { $found = $true; break }
                }
                if (-not $found) { $missingCfg += $h }
            }
            if ($missingCfg.Count -gt 0) {
                if ($storagePreseedSkipped) {
                    $details += ("NOTE: storage pre-seeding was skipped by setup.cmd (/skipstorage). config\\devices.cmd virtio-blk HWIDs not found in any packaged INF: " + ($missingCfg -join ", ") + " (expected for partial driver payloads).")
                } else {
                    $status = Merge-Status $status "WARN"
                    $details += ("config\\devices.cmd virtio-blk HWIDs not found in any packaged INF: " + ($missingCfg -join ", ") + ". Media may be the wrong version/arch.")
                }
            }
        }

        $packagedDriversSummary = $summary
        $report.packaged_drivers_summary = $summary

        $sumText = "Media driver inventory: arch=" + $arch + ", driver folders=" + $summary.total_driver_folders + ", INFs=" + $summary.total_inf_files + ", HWID prefixes=" + $summary.total_hwid_prefixes
        if ($summary.total_inf_files -eq 0) {
            $status = "WARN"
            $details += "No .inf files found under drivers\\" + $arch + ". Driver installation from this media will fail."
        }
        if ($summary.inf_parse_failures -gt 0) {
            $status = Merge-Status $status "WARN"
            $details += ($summary.inf_parse_failures.ToString() + " INF file(s) could not be parsed (best-effort).")
        }

        # High-signal per-folder summary lines (avoid dumping raw INF contents).
        foreach ($pkg in $summary.driver_folders) {
            if (-not $pkg.infs -or $pkg.infs.Count -eq 0) {
                $details += ($pkg.name + ": INFs=0 (sys=" + $pkg.sys_count + ", cat=" + $pkg.cat_count + ")")
                continue
            }

            foreach ($inf in $pkg.infs) {
                $line = $pkg.name + ": " + $inf.inf_rel_path
                if ($inf.provider) { $line += ", Provider=" + $inf.provider }
                if ($inf.driver_ver_raw) { $line += ", DriverVer=" + $inf.driver_ver_raw }
                if ($inf.hwid_prefixes -and $inf.hwid_prefixes.Count -gt 0) {
                    $hwids = $inf.hwid_prefixes
                    if ($hwids.Count -le 6) {
                        $line += ", HWIDs=" + ($hwids -join ", ")
                    } else {
                        $preview = @($hwids | Select-Object -First 6)
                        $line += ", HWIDs=" + ($preview -join ", ") + " ... (" + $hwids.Count + " total)"
                    }
                }
                if ($inf.add_services -and $inf.add_services.Count -gt 0) { $line += ", AddService=" + ($inf.add_services -join ",") }
                $details += $line
            }
        }
    }

    $report.packaged_drivers_summary = $summary
    Add-Check "packaged_drivers_summary" "Packaged Drivers (media INFs)" $status $sumText $summary $details
} catch {
    $report.packaged_drivers_summary = @{
        guest_tools_root = $scriptDir
        error = $_.Exception.Message
    }
    Add-Check "packaged_drivers_summary" "Packaged Drivers (media INFs)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Clock sanity (certificate validity depends on correct time) ---
try {
    $now = Get-Date
    $year = $now.Year
    $minYear = 2015
    $maxYear = 2100

    $clockStatus = "PASS"
    $clockSummary = "System clock: " + $now.ToString()
    $clockDetails = @()
    if (($year -lt $minYear) -or ($year -gt $maxYear)) {
        $clockStatus = "WARN"
        $clockSummary = "System clock looks wrong: " + $now.ToString()
        $clockDetails += "Set correct date/time; incorrect clock can break signature verification (certificates may appear not-yet-valid or expired)."
        $clockDetails += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-52-signature-and-trust-failures"
    }

    $clockData = @{
        now_local = $now.ToString("o")
        year = $year
        min_year = $minYear
        max_year = $maxYear
    }
    Add-Check "clock_sanity" "Clock Sanity (signature validity)" $clockStatus $clockSummary $clockData $clockDetails
} catch {
    Add-Check "clock_sanity" "Clock Sanity (signature validity)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Hotfix: KB3033929 (SHA-256 / SHA-2 signature support) ---
try {
    $kb = Try-GetWmi "Win32_QuickFixEngineering" "HotFixID='KB3033929'"
    $installed = $false
    $kbInfo = $null
    if ($kb) {
        $installed = $true
        $one = $kb | Select-Object -First 1
        $kbInfo = @{
            hotfix_id = "" + $one.HotFixID
            description = "" + $one.Description
            installed_on = "" + $one.InstalledOn
            installed_by = "" + $one.InstalledBy
        }
    } else {
        $kbInfo = @{
            hotfix_id = "KB3033929"
            installed = $false
        }
    }

    $kbStatus = "PASS"
    $kbSummary = ""
    $kbDetails = @()

    if ($installed) {
        $kbSummary = "KB3033929 is installed."
    } else {
        $kbSummary = "KB3033929 is NOT installed."
        $kbStatus = "WARN"
        $kbDetails += "Windows 7 may require KB3033929 to validate SHA-256-signed driver catalogs (otherwise Device Manager Code 52)."
        $kbDetails += "Install KB3033929 (x86/x64) and reboot."
        $kbDetails += "See: docs/windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support"
    }

    Add-Check "kb3033929" "Hotfix: KB3033929 (SHA-256 signatures)" $kbStatus $kbSummary $kbInfo $kbDetails
} catch {
    Add-Check "kb3033929" "Hotfix: KB3033929 (SHA-256 signatures)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Hotfix: KB4474419 (SHA-2 code signing support update) ---
try {
    $kb = Try-GetWmi "Win32_QuickFixEngineering" "HotFixID='KB4474419'"
    $installed = $false
    $kbInfo = $null
    if ($kb) {
        $installed = $true
        $one = $kb | Select-Object -First 1
        $kbInfo = @{
            hotfix_id = "" + $one.HotFixID
            description = "" + $one.Description
            installed_on = "" + $one.InstalledOn
            installed_by = "" + $one.InstalledBy
        }
    } else {
        $kbInfo = @{
            hotfix_id = "KB4474419"
            installed = $false
        }
    }

    $kbStatus = "PASS"
    $kbSummary = ""
    $kbDetails = @()

    if ($installed) {
        $kbSummary = "KB4474419 is installed."
    } else {
        $kbSummary = "KB4474419 is NOT installed."
        $kbStatus = "WARN"
        $kbDetails += "Windows 7 may require KB4474419 (SHA-2 support update) to validate newer SHA-2 signatures (otherwise Device Manager Code 52)."
        $kbDetails += "Install KB4474419 and reboot (KB4490628 is a common prerequisite)."
        $kbDetails += "See: docs/windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support"
    }

    Add-Check "kb4474419" "Hotfix: KB4474419 (SHA-2 signatures)" $kbStatus $kbSummary $kbInfo $kbDetails
} catch {
    Add-Check "kb4474419" "Hotfix: KB4474419 (SHA-2 signatures)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Hotfix: KB4490628 (servicing stack prerequisite) ---
try {
    $kb = Try-GetWmi "Win32_QuickFixEngineering" "HotFixID='KB4490628'"
    $installed = $false
    $kbInfo = $null
    if ($kb) {
        $installed = $true
        $one = $kb | Select-Object -First 1
        $kbInfo = @{
            hotfix_id = "" + $one.HotFixID
            description = "" + $one.Description
            installed_on = "" + $one.InstalledOn
            installed_by = "" + $one.InstalledBy
        }
    } else {
        $kbInfo = @{
            hotfix_id = "KB4490628"
            installed = $false
        }
    }

    $kbStatus = "PASS"
    $kbSummary = ""
    $kbDetails = @()

    if ($installed) {
        $kbSummary = "KB4490628 is installed."
    } else {
        $kbSummary = "KB4490628 is NOT installed."
        $kbStatus = "WARN"
        $kbDetails += "KB4490628 is a common servicing stack prerequisite for installing KB4474419 (SHA-2 support update)."
        $kbDetails += "Install KB4490628, then install KB4474419, then reboot."
        $kbDetails += "See: docs/windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support"
    }

    Add-Check "kb4490628" "Hotfix: KB4490628 (Servicing Stack)" $kbStatus $kbSummary $kbInfo $kbDetails
} catch {
    Add-Check "kb4490628" "Hotfix: KB4490628 (Servicing Stack)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Certificate store (driver signing trust) ---
try {
    $certsRequired = $null
    $signingPolicy = $null
    if ($report.media_integrity -and ($report.media_integrity -is [hashtable]) -and $report.media_integrity.ContainsKey("parse_ok") -and ($report.media_integrity.parse_ok -eq $true)) {
        if ($report.media_integrity.ContainsKey("signing_policy") -and $report.media_integrity.signing_policy) {
            $signingPolicy = "" + $report.media_integrity.signing_policy
        }
        if ($report.media_integrity.ContainsKey("certs_required") -and ($report.media_integrity.certs_required -ne $null)) {
            # manifest.json uses a JSON boolean; Parse-JsonCompat returns a native boolean.
            $certsRequired = [bool]$report.media_integrity.certs_required
        }
        if (($certsRequired -eq $null) -and $signingPolicy -and ($signingPolicy.ToLower() -eq "none")) {
            $certsRequired = $false
        }
    }

    $certSearchDirs = @($scriptDir)
    $certDir = Join-Path $scriptDir "certs"
    if (Test-Path $certDir) { $certSearchDirs += $certDir }

    $certFiles = @()
    foreach ($dir in $certSearchDirs) {
        $certFiles += (Get-ChildItem -Path $dir -Filter *.cer -ErrorAction SilentlyContinue)
        $certFiles += (Get-ChildItem -Path $dir -Filter *.crt -ErrorAction SilentlyContinue)
        $certFiles += (Get-ChildItem -Path $dir -Filter *.p7b -ErrorAction SilentlyContinue)
    }

    # If Guest Tools setup was run, it records installed cert thumbprints under
    # C:\AeroGuestTools\installed-certs.txt. Use this as an additional/alternate
    # signal when the verify script isn't running next to the original cert files.
    $setupCertChecks = @()
    if ($installedCertThumbprints -and $installedCertThumbprints.Count -gt 0) {
        foreach ($tp in $installedCertThumbprints) {
            if (-not $tp) { continue }
            $rootLM = Find-CertInStore $tp "Root" "LocalMachine"
            $pubLM = Find-CertInStore $tp "TrustedPublisher" "LocalMachine"
            $setupCertChecks += @{
                thumbprint = $tp
                local_machine_root = $rootLM
                local_machine_trusted_publisher = $pubLM
            }
        }
    }

    $certResults = @()
    foreach ($cf in $certFiles) {
        $certsInFile = Load-CertsFromFile $cf.FullName
        if (-not $certsInFile -or $certsInFile.Count -eq 0) {
            $certResults += @{
                file = $cf.Name
                path = $cf.FullName
                status = "WARN"
                error = "Unable to load any certificates from file."
            }
            continue
        }

        $idx = 0
        foreach ($cert in $certsInFile) {
            $thumb = "" + $cert.Thumbprint
            $subj = "" + $cert.Subject
            $rootLM = Find-CertInStore $thumb "Root" "LocalMachine"
            $pubLM = Find-CertInStore $thumb "TrustedPublisher" "LocalMachine"

            $certResults += @{
                file = $cf.Name
                path = $cf.FullName
                cert_index = $idx
                thumbprint = $thumb
                subject = $subj
                not_after = $cert.NotAfter.ToUniversalTime().ToString("o")
                local_machine_root = $rootLM
                local_machine_trusted_publisher = $pubLM
            }
            $idx++
        }
    }

    $certStatus = "PASS"
    $certSummary = ""
    $certDetails = @()

    $missingSetupThumbprints = @($setupCertChecks | Where-Object { (-not $_.local_machine_root) -or (-not $_.local_machine_trusted_publisher) })

    if ((-not $certFiles -or $certFiles.Count -eq 0) -and $setupCertChecks.Count -eq 0) {
        if ($certsRequired -eq $false) {
            $certStatus = "PASS"
            $certSummary = ("No certificate files found under Guest Tools root/certs, and none are required by signing_policy=" + $signingPolicy + ".")
            $certDetails += "If you are using custom-signed/test-signed drivers, rebuild Guest Tools with a cert under certs\\ and signing_policy=test."
        } else {
            $certStatus = "WARN"
            $certSummary = "No certificate files found under Guest Tools root/certs and no installed cert list found; unable to verify certificate store."
            $certDetails += "Run verify.cmd from the Guest Tools media (or copy the full ISO contents) so certs\\ is present."
        }
    } elseif (-not $certFiles -or $certFiles.Count -eq 0) {
        $certSummary = "No certificate files found under Guest Tools root/certs; verifying only certificates recorded by setup.cmd."
        if ($missingSetupThumbprints.Count -gt 0) {
            $certStatus = "WARN"
            $certDetails += ($missingSetupThumbprints.Count.ToString() + " certificate(s) recorded by setup.cmd are not installed in both LocalMachine Root + TrustedPublisher stores.")
        }
    } else {
        $badCount = 0
        $missingCount = 0
        foreach ($cr in $certResults) {
            if ($cr.status -eq "WARN") {
                $badCount++
                continue
            }
            if (-not $cr.local_machine_root -or -not $cr.local_machine_trusted_publisher) {
                $missingCount++
            }
        }

        $certSummary = "Certificate file(s) found: " + $certFiles.Count + "; certificates parsed: " + (@($certResults | Where-Object { $_.thumbprint }).Count) + "; setup-installed certs: " + $setupCertChecks.Count
        if ($badCount -gt 0 -or $missingCount -gt 0 -or $missingSetupThumbprints.Count -gt 0) {
            $certStatus = "WARN"
            if ($badCount -gt 0) { $certDetails += ($badCount.ToString() + " certificate file(s) could not be parsed.") }
            if ($missingCount -gt 0) { $certDetails += ($missingCount.ToString() + " certificate(s) are not installed in both LocalMachine Root + TrustedPublisher stores.") }
            if ($missingSetupThumbprints.Count -gt 0) { $certDetails += ($missingSetupThumbprints.Count.ToString() + " certificate(s) recorded by setup.cmd are not installed in both LocalMachine Root + TrustedPublisher stores.") }
            $certDetails += "Re-run Guest Tools setup as Administrator to install the driver certificate(s)."
        }
    }

    $certData = @{
        script_dir = $scriptDir
        search_dirs = $certSearchDirs
        cert_files = @($certFiles | ForEach-Object { $_.FullName })
        signing_policy = $signingPolicy
        certs_required = $certsRequired
        certificates = $certResults
        installed_certs_from_setup = $setupCertChecks
    }
    Add-Check "certificate_store" "Certificate Store (driver signing trust)" $certStatus $certSummary $certData $certDetails
} catch {
    Add-Check "certificate_store" "Certificate Store (driver signing trust)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Signature mode (bcdedit) ---
try {
    $bcd = Invoke-Capture "bcdedit.exe" @("/enum","{current}")
    if ($bcd.exit_code -ne 0 -or -not $bcd.output) {
        # Fallback to full enumeration if {current} is unavailable (or bcdedit behaves differently).
        $bcd = Invoke-Capture "bcdedit.exe" @("/enum")
    }
    $raw = $bcd.output
    $testsigning = $null
    $nointegritychecks = $null

    foreach ($line in $raw -split "`r?`n") {
        if ($line -match '(?i)\btestsigning\b') {
            $parts = ($line.Trim() -split '\s+')
            $testsigning = $parts[$parts.Length - 1]
        }
        if ($line -match '(?i)\bnointegritychecks\b') {
            $parts = ($line.Trim() -split '\s+')
            $nointegritychecks = $parts[$parts.Length - 1]
        }
    }

    $sigData = @{
        bcdedit_exit_code = $bcd.exit_code
        bcdedit_raw = $raw
        signing_policy = $gtSigningPolicy
        certs_required = $gtCertsRequired
        testsigning = $testsigning
        nointegritychecks = $nointegritychecks
    }

    $sigStatus = "PASS"
    $sigSummary = ""
    $sigDetails = @()

    if ($bcd.exit_code -ne 0 -or -not $raw) {
        $sigStatus = "WARN"
        $sigSummary = "bcdedit failed or produced no output (run as Administrator for full results)."
        $sigDetails += "Exit code: " + $bcd.exit_code
    } else {
        $policy = $null
        if ($report.media_integrity -and ($report.media_integrity -is [hashtable]) -and $report.media_integrity.signing_policy) {
            $policy = "" + $report.media_integrity.signing_policy
        }
        $policyLower = ""
        if ($policy) { $policyLower = $policy.ToLower() }

        if (-not $testsigning) { $testsigning = "Unknown/NotSet" }
        if (-not $nointegritychecks) { $nointegritychecks = "Unknown/NotSet" }
        $sigSummary = "testsigning=" + $testsigning + ", nointegritychecks=" + $nointegritychecks

        # Interpret values loosely (locales vary).
        $tsOn = ($testsigning -match '^(?i)(yes|on|true|1)$')
        $nicOn = ($nointegritychecks -match '^(?i)(yes|on|true|1)$')
        $is64 = $false
        if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
            $is64 = ("" + $report.checks.os.data.architecture) -match '64'
        } else {
            $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
        }

        $policy = $gtSigningPolicy
        if (-not $policy) { $policy = "unknown" }
        $policyLower = ("" + $policy).ToLower()

        if ($nicOn) {
            $sigStatus = Merge-Status $sigStatus "WARN"
            $sigDetails += "nointegritychecks is enabled. This is not recommended; prefer testsigning or properly signed drivers."
        }

        if ($is64) {
            if (($policyLower -eq "production") -or ($policyLower -eq "none")) {
                if ($tsOn) {
                    $sigStatus = Merge-Status $sigStatus "WARN"
                    $sigDetails += ("signing_policy=" + $policyLower + " but testsigning is enabled. Consider disabling it: bcdedit /set testsigning off (then reboot).")
                }
            } elseif ($policyLower -eq "test") {
                if (-not $tsOn) {
                    if ($nicOn) {
                        $sigStatus = Merge-Status $sigStatus "WARN"
                        $sigDetails += "signing_policy=test but testsigning is not enabled. nointegritychecks is enabled, so drivers may still load, but this is not recommended."
                    } else {
                        $sigStatus = Merge-Status $sigStatus "FAIL"
                        $sigDetails += "signing_policy=test but testsigning is not enabled. Enable it: bcdedit /set testsigning on (then reboot)."
                    }
                }
            } else {
                # Unknown policy: keep legacy guidance (best-effort).
                if (-not $tsOn) {
                    $sigStatus = Merge-Status $sigStatus "WARN"
                    $sigDetails += "testsigning is not enabled. If Aero drivers are test-signed (common on Windows 7 x64), enable it: bcdedit /set testsigning on (then reboot)."
                }
            }
        }
    }

    Add-Check "signature_mode" "Signature Mode (BCDEdit)" $sigStatus $sigSummary $sigData $sigDetails
} catch {
    Add-Check "signature_mode" "Signature Mode (BCDEdit)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Driver packages (pnputil -e) ---
try {
    $pnp = Invoke-Capture "pnputil.exe" @("-e")
    $raw = $pnp.output

    $keywords = @(
        "aero",
        "virtio",
        "viostor",
        "vionet",
        "netkvm",
        "viogpu",
        "vioinput",
        "virtioinput",
        "viosnd",
        "aerosnd",
        "virtiosnd",
        "aeroviosnd",
        "aeroviosnd_legacy",
        "aerovblk",
        "aerovnet",
        "aero_virtio_blk",
        "aero_virtio_net",
        "aero_virtio_input",
        "aero_virtio_snd",
        "1af4"
    )
    foreach ($s in @($cfgVirtioBlkService,$cfgVirtioNetService,$cfgVirtioSndService,$cfgVirtioInputService,$cfgGpuService)) {
        if ($s -and ("" + $s).Trim().Length -gt 0) {
            $kw = ("" + $s).Trim().ToLower()
            if (-not ($keywords -contains $kw)) { $keywords += $kw }
        }
    }
    $blocks = @()
    if ($raw) {
        # Split on blank lines (package blocks).
        $tmp = $raw -split "\r?\n\r?\n+"
        foreach ($b in $tmp) {
            $bb = $b.Trim()
            if ($bb.Length -eq 0) { continue }
            if ($bb -match '(?i)oem\d+\.inf') { $blocks += $bb }
        }
    }

    $packages = @()
    foreach ($b in $blocks) {
        $published = $null
        $m = [regex]::Match($b, '(?i)\boem\d+\.inf\b')
        if ($m.Success) { $published = $m.Value }

        $provider = $null
        $class = $null
        $driverDateAndVersion = $null
        $signerName = $null
        foreach ($line in $b -split "`r?`n") {
            $t = $line.Trim()
            if ($t -match '(?i)^driver package provider\s*:\s*(.+)$') { $provider = $matches[1].Trim(); continue }
            if ($t -match '(?i)^class\s*:\s*(.+)$') { $class = $matches[1].Trim(); continue }
            if ($t -match '(?i)^driver date and version\s*:\s*(.+)$') { $driverDateAndVersion = $matches[1].Trim(); continue }
            if ($t -match '(?i)^signer name\s*:\s*(.+)$') { $signerName = $matches[1].Trim(); continue }
        }

        $isInstalledByGuestTools = $false
        if ($published -and $gtInstalledDriverPackages -and $gtInstalledDriverPackages.Count -gt 0) {
            foreach ($p in $gtInstalledDriverPackages) {
                if ($p -and ($p.ToLower() -eq $published.ToLower())) { $isInstalledByGuestTools = $true; break }
            }
        }

        $isAero = $false
        $lower = $b.ToLower()
        foreach ($kw in $keywords) {
            if ($lower.Contains($kw)) { $isAero = $true; break }
        }
        if ($isInstalledByGuestTools) { $isAero = $true }

        $packages += @{
            published_name = $published
            is_aero_related = $isAero
            is_installed_by_guest_tools = $isInstalledByGuestTools
            class = $class
            provider = $provider
            driver_date_and_version = $driverDateAndVersion
            signer_name = $signerName
            raw_block = $b
        }
    }

    $aeroPackages = @($packages | Where-Object { $_.is_aero_related })

    $aeroInstalledByGt = @($aeroPackages | Where-Object { $_.is_installed_by_guest_tools })

    $missingGtPackages = @()
    if ($gtInstalledDriverPackages -and $gtInstalledDriverPackages.Count -gt 0) {
        $present = @{}
        foreach ($pkg in $packages) {
            if ($pkg.published_name) { $present[$pkg.published_name.ToLower()] = $true }
        }
        foreach ($p in $gtInstalledDriverPackages) {
            if (-not $p) { continue }
            $k = $p.ToLower()
            if (-not $present.ContainsKey($k)) { $missingGtPackages += $p }
        }
    }

    $drvData = @{
        pnputil_exit_code = $pnp.exit_code
        pnputil_raw = $raw
        total_packages_parsed = $packages.Count
        aero_packages = $aeroPackages
        aero_packages_installed_by_guest_tools = $aeroInstalledByGt
        guest_tools_installed_driver_packages = $gtInstalledDriverPackages
        guest_tools_driver_packages_missing = $missingGtPackages
        match_keywords = $keywords
    }

    $drvStatus = "PASS"
    $drvSummary = ""
    $drvDetails = @()
    if ($pnp.exit_code -ne 0 -or -not $raw) {
        $drvStatus = "WARN"
        $drvSummary = "pnputil failed or produced no output."
        $drvDetails += "Exit code: " + $pnp.exit_code
    } else {
        $drvSummary = "Detected " + $aeroPackages.Count + " Aero-related driver package(s) (installed-by-GuestTools: " + $aeroInstalledByGt.Count + "; missing-from-pnputil: " + $missingGtPackages.Count + "; parsed " + $packages.Count + " total)."
        if ($aeroPackages.Count -eq 0) {
            $drvStatus = "WARN"
            $drvDetails += "No Aero-related packages matched heuristic keywords. See pnputil_raw in report.json."
        }
        if ($missingGtPackages.Count -gt 0) {
            $drvStatus = "WARN"
            $drvDetails += ($missingGtPackages.Count.ToString() + " driver package(s) recorded by setup.cmd are not present in pnputil -e output: " + ($missingGtPackages -join ", "))
        }
    }
    Add-Check "driver_packages" "Driver Packages (pnputil -e)" $drvStatus $drvSummary $drvData $drvDetails
} catch {
    Add-Check "driver_packages" "Driver Packages (pnputil -e)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Bound devices (WMI Win32_PnPEntity; optional devcon) ---
$boundDevicesForCorrelation = $null
try {
    $devconDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $devconPath = Join-Path $devconDir "devcon.exe"

    $svcCandidates = @(
        "viostor",
        "aeroviostor",
        "aero_virtio_blk",
        "aerovblk",
        "virtio_blk",
        "virtio-blk",
        "aero_virtio_net",
        "aerovnet",
        "vionet",
        "netkvm",
        "viogpu",
        "AeroGPU",
        "aerogpu",
        "aero-gpu",
        "aero_virtio_snd",
        "viosnd",
        "aerosnd",
        "virtiosnd",
        "aeroviosnd",
        "aeroviosnd_legacy",
        "aero_virtio_input",
        "vioinput",
        "virtioinput",
        "aerovioinput"
    )
    foreach ($s in @($cfgVirtioBlkService,$cfgVirtioNetService,$cfgVirtioSndService,$cfgVirtioInputService,$cfgGpuService)) {
        if ($s -and ("" + $s).Trim().Length -gt 0) { $svcCandidates = @((("" + $s).Trim())) + $svcCandidates }
    }
    $svcCandidates = Dedup-CaseInsensitive $svcCandidates
    $kw = @("aero","virtio")

    $signedDriverMap = @{}
    $signedDrivers = Try-GetWmi "Win32_PnPSignedDriver" ""
    if ($signedDrivers) {
        foreach ($sd in $signedDrivers) {
            $id = "" + $sd.DeviceID
            if (-not $id -or $id.Length -eq 0) { continue }
            $signedDriverMap[$id.ToUpper()] = @{
                device_name = "" + $sd.DeviceName
                inf_name = "" + $sd.InfName
                driver_version = "" + $sd.DriverVersion
                driver_date = "" + $sd.DriverDate
                driver_provider_name = "" + $sd.DriverProviderName
                is_signed = $sd.IsSigned
                signer = "" + $sd.Signer
                manufacturer = "" + $sd.Manufacturer
                friendly_name = "" + $sd.FriendlyName
                driver_class = "" + $sd.DriverClass
            }
        }
    }

    $devices = @()
    $pnp = Try-GetWmi "Win32_PnPEntity" ""
    if ($pnp) {
        foreach ($d in $pnp) {
            $name = "" + $d.Name
            $mfr = "" + $d.Manufacturer
            $pnpid = "" + $d.PNPDeviceID
            $svc = "" + $d.Service

            $relevant = $false
            if ($pnpid) {
                foreach ($rx in @($cfgVirtioBlkRegex,$cfgVirtioNetRegex,$cfgVirtioSndRegex,$cfgVirtioInputRegex,$cfgGpuRegex)) {
                    if ($rx -and $pnpid -match $rx) { $relevant = $true; break }
                }
            }
            if (-not $relevant) {
                foreach ($k in $kw) {
                    if (($name.ToLower().Contains($k)) -or ($mfr.ToLower().Contains($k))) { $relevant = $true; break }
                }
            }
            if (-not $relevant -and $svc) {
                foreach ($s in $svcCandidates) {
                    if ($svc.ToLower() -eq $s.ToLower()) { $relevant = $true; break }
                }
            }

            if ($relevant) {
                $err = $d.ConfigManagerErrorCode
                $errMeaning = $null
                if ($err -ne $null) { $errMeaning = Get-ConfigManagerErrorMeaning $err }

                $signed = $null
                if ($pnpid) {
                    $key = $pnpid.ToUpper()
                    if ($signedDriverMap.ContainsKey($key)) { $signed = $signedDriverMap[$key] }
                }

                $devices += @{
                    name = $name
                    manufacturer = $mfr
                    pnp_device_id = $pnpid
                    service = $svc
                    status = "" + $d.Status
                    pnp_class = "" + $d.PNPClass
                    config_manager_error_code = $err
                    config_manager_error_meaning = $errMeaning
                    class_guid = "" + $d.ClassGuid
                    signed_driver = $signed
                }
            }
        }
    }

    # Preserve for later correlation against packaged media drivers.
    $boundDevicesForCorrelation = $devices

    $devcon = $null
    if (Test-Path $devconPath) {
        $devcon = Invoke-Capture $devconPath @("findall","*")
    }

    $devStatus = "PASS"
    $devSummary = ""
    $devDetails = @()
    if (-not $pnp) {
        $devStatus = "WARN"
        $devSummary = "Unable to query Win32_PnPEntity."
    } else {
        $devSummary = "Detected " + $devices.Count + " Aero-related device(s) (heuristic)."
        if ($devices.Count -eq 0) {
            $devStatus = "WARN"
            $devDetails += "No Aero-related devices matched heuristic filters."
        }
        $bad = @($devices | Where-Object { $_.config_manager_error_code -ne $null -and $_.config_manager_error_code -ne 0 })
        if ($bad.Count -gt 0) {
            $devStatus = Merge-Status $devStatus "WARN"
            $devDetails += ($bad.Count.ToString() + " device(s) report ConfigManagerErrorCode != 0 (driver binding/problem).")
        }
        $code52 = @($devices | Where-Object { $_.config_manager_error_code -eq 52 })
        if ($code52.Count -gt 0) {
            $devStatus = Merge-Status $devStatus "WARN"
            $devDetails += ($code52.Count.ToString() + " device(s) report Code 52 (signature/trust failure). Review Signature Mode + Certificate Store + KB3033929 checks.")
            $devDetails += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-52-signature-and-trust-failures"
        }
        $code28 = @($devices | Where-Object { $_.config_manager_error_code -eq 28 })
        if ($code28.Count -gt 0) {
            $devStatus = Merge-Status $devStatus "WARN"
            $devDetails += ($code28.Count.ToString() + " device(s) report Code 28 (drivers not installed). Re-run Guest Tools setup / update driver in Device Manager.")
            $devDetails += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-28-drivers-not-installed"
        }
        $code10 = @($devices | Where-Object { $_.config_manager_error_code -eq 10 })
        if ($code10.Count -gt 0) {
            $devStatus = Merge-Status $devStatus "WARN"
            $devDetails += ($code10.Count.ToString() + " device(s) report Code 10 (device cannot start).")
            $devDetails += "See: docs/windows7-driver-troubleshooting.md#issue-device-manager-code-10-device-cannot-start"
        }
    }

    $devData = @{
        devices = $devices
        used_devcon = (Test-Path $devconPath)
        devcon_path = (if (Test-Path $devconPath) { $devconPath } else { $null })
        devcon_exit_code = (if ($devcon) { $devcon.exit_code } else { $null })
        devcon_raw = (if ($devcon) { $devcon.output } else { $null })
    }
    Add-Check "bound_devices" "Bound Devices (WMI Win32_PnPEntity)" $devStatus $devSummary $devData $devDetails

    # --- Installed driver signatures (Win32_PnPSignedDriver) ---
    # This complements ConfigManagerErrorCode (e.g. Code 52) by reporting whether the currently
    # bound driver package is actually signed (IsSigned) and what signer/provider WMI reports.
    #
    # WARNING/PASS semantics:
    # - PASS when no relevant devices are present (skip) OR all relevant devices with a signed_driver record are signed.
    # - WARN when any relevant device is unsigned, or when signer/provider metadata is missing (high-signal for trust issues).
    try {
        $sigStatus = "PASS"
        $sigSummary = ""
        $sigDetails = @()

        $is64 = $false
        if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
            $is64 = ("" + $report.checks.os.data.architecture) -match '64'
        } else {
            $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
        }

        $policy = $gtSigningPolicy
        if (-not $policy -and $report.media_integrity -and ($report.media_integrity -is [hashtable]) -and $report.media_integrity.signing_policy) {
            $policy = "" + $report.media_integrity.signing_policy
        }
        $policyLower = ""
        if ($policy) { $policyLower = ("" + $policy).Trim().ToLower() }
        if (-not $policyLower -or $policyLower.Length -eq 0) { $policyLower = "unknown" }

        $signingExpected = $false
        if ($policyLower -eq "production" -or $policyLower -eq "test") {
            $signingExpected = $true
        } elseif ($policyLower -eq "none") {
            $signingExpected = $false
        } else {
            # Unknown policy: on Win7 x64 assume signing is expected unless signature enforcement is bypassed.
            $signingExpected = $is64
        }

        $sigData = @{
            relevant_devices = (if ($devices) { $devices.Count } else { 0 })
            devices_with_signed_driver = 0
            unsigned_devices = 0
            missing_provider = 0
            missing_signer = 0
            unknown_is_signed = 0
            signing_policy = $policyLower
            signing_expected = $signingExpected
            is_64bit = $is64
            issue_devices = @()
        }

        if (-not $devices -or $devices.Count -eq 0) {
            # Don't add additional WARN noise; "bound_devices" already reports this condition.
            $sigSummary = "Skipped: no Aero-related devices detected."
        } else {
            $withSd = @($devices | Where-Object { $_.signed_driver })
            $sigData.devices_with_signed_driver = $withSd.Count

            if ($withSd.Count -eq 0) {
                $sigStatus = "WARN"
                $sigSummary = "No Win32_PnPSignedDriver records matched relevant devices; cannot verify signature state."
                $sigDetails += "Run as Administrator and ensure the WMI service is functioning."
            } else {
                foreach ($d in $withSd) {
                    $sd = $d.signed_driver

                    $rawIsSigned = (if ($sd.ContainsKey("is_signed")) { $sd.is_signed } else { $null })
                    $isSigned = $null
                    if ($rawIsSigned -eq $true) { $isSigned = $true }
                    elseif ($rawIsSigned -eq $false) { $isSigned = $false }
                    elseif ($rawIsSigned -ne $null) {
                        $t = ("" + $rawIsSigned).Trim().ToLower()
                        if ($t -match '^(true|yes|on|1)$') { $isSigned = $true }
                        elseif ($t -match '^(false|no|off|0)$') { $isSigned = $false }
                    }

                    $inf = $null
                    $provider = $null
                    $signer = $null
                    if ($sd) {
                        if ($sd.inf_name) { $inf = ("" + $sd.inf_name).Trim() }
                        if ($sd.driver_provider_name) { $provider = ("" + $sd.driver_provider_name).Trim() }
                        if ($sd.signer) { $signer = ("" + $sd.signer).Trim() }
                    }

                    $missingProvider = (-not $provider -or $provider.Length -eq 0)
                    $missingSigner = (-not $signer -or $signer.Length -eq 0)
                    $unsigned = ($isSigned -eq $false)
                    $unknownSigned = ($isSigned -eq $null)

                    if ($unsigned) { $sigData.unsigned_devices++ }
                    if ($missingProvider) { $sigData.missing_provider++ }
                    if ($missingSigner) { $sigData.missing_signer++ }
                    if ($unknownSigned) { $sigData.unknown_is_signed++ }

                    if ($unsigned -or $unknownSigned -or $missingProvider -or $missingSigner) {
                        $sigData.issue_devices += @{
                            name = "" + $d.name
                            pnp_device_id = "" + $d.pnp_device_id
                            inf_name = $inf
                            is_signed = $isSigned
                            driver_provider_name = $provider
                            signer = $signer
                            missing_provider = $missingProvider
                            missing_signer = $missingSigner
                        }

                        $line = "" + $d.name
                        if ($d.pnp_device_id) { $line += " (PNPDeviceID=" + $d.pnp_device_id + ")" }
                        if ($inf) { $line += ", INF=" + $inf }
                        $line += ", IsSigned=" + (if ($isSigned -eq $null) { "Unknown" } else { $isSigned })
                        $line += ", Provider=" + (if ($provider) { $provider } else { "<missing>" })
                        $line += ", Signer=" + (if ($signer) { $signer } else { "<missing>" })
                        $sigDetails += $line
                    }
                }

                $hasIssues = (($sigData.unsigned_devices -gt 0) -or ($sigData.unknown_is_signed -gt 0) -or ($sigData.missing_provider -gt 0) -or ($sigData.missing_signer -gt 0))
                if (-not $hasIssues) {
                    $sigStatus = "PASS"
                    $sigSummary = "All relevant devices with signed driver data report IsSigned=True (" + $withSd.Count + " device(s))."
                } else {
                    if ($signingExpected -and $sigData.unsigned_devices -gt 0) {
                        $sigStatus = "FAIL"
                    } else {
                        $sigStatus = "WARN"
                    }

                    $sigSummary = "Signature issues: unsigned=" + $sigData.unsigned_devices + ", unknown IsSigned=" + $sigData.unknown_is_signed + ", missing provider=" + $sigData.missing_provider + ", missing signer=" + $sigData.missing_signer + " (devices with signed driver data: " + $withSd.Count + "; signing_policy=" + $policyLower + ", signing_expected=" + $signingExpected + ")."
                    $sigDetails += "Remediation:"
                    $sigDetails += "  - If signing_policy=test: re-run Guest Tools setup as Administrator to install the driver certificate(s) (see Certificate Store check)."
                    $sigDetails += "  - Verify SHA-256 support hotfixes are installed (KB3033929; KB4474419 once available)."
                    $sigDetails += "  - Verify the system clock/timezone is correct (invalid clocks can break signature validation)."
                    if ($policyLower -eq "none") {
                        $sigDetails += "  - Note: signing_policy=none implies signature enforcement may be bypassed (nointegritychecks). This is not recommended for general use."
                    }
                }
            }
        }

        Add-Check "installed_driver_signatures" "Installed Driver Signatures (Win32_PnPSignedDriver)" $sigStatus $sigSummary $sigData $sigDetails
    } catch {
        Add-Check "installed_driver_signatures" "Installed Driver Signatures (Win32_PnPSignedDriver)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
    }

    # Per-device-class binding checks (best-effort).
    # These are intentionally WARN (not FAIL) when missing, since the guest might still be
    # using baseline devices (AHCI/e1000/VGA/PS2) even if Guest Tools are installed.

    $blkRegex = $cfgVirtioBlkRegex
    $netRegex = $cfgVirtioNetRegex
    $sndRegex = $cfgVirtioSndRegex
    $inputRegex = $cfgVirtioInputRegex
    $gpuRegex = $cfgGpuRegex

    $storageServiceCandidates = @("aero_virtio_blk","aerovblk","viostor","aeroviostor","virtio_blk","virtio-blk","aerostor","aeroblk")
    if ($cfgVirtioBlkService) { $storageServiceCandidates = @($cfgVirtioBlkService) + $storageServiceCandidates }
    $networkServiceCandidates = @("aero_virtio_net","aerovnet","vionet","netkvm")
    if ($cfgVirtioNetService) { $networkServiceCandidates = @($cfgVirtioNetService) + $networkServiceCandidates }
    $graphicsServiceCandidates = @("AeroGPU","viogpu","aerogpu","aero-gpu")
    if ($cfgGpuService) { $graphicsServiceCandidates = @($cfgGpuService) + $graphicsServiceCandidates }
    $audioServiceCandidates = @("aero_virtio_snd","aeroviosnd_legacy","aeroviosnd_ioport","aeroviosnd","viosnd","aerosnd","virtiosnd")
    if ($cfgVirtioSndService) { $audioServiceCandidates = @($cfgVirtioSndService) + $audioServiceCandidates }
    $audioServiceCandidates = Dedup-CaseInsensitive $audioServiceCandidates

    $inputServiceCandidates = @("aero_virtio_input","virtioinput","vioinput","aeroinput","aerovioinput")
    if ($cfgVirtioInputService) { $inputServiceCandidates = @($cfgVirtioInputService) + $inputServiceCandidates }
    $inputServiceCandidates = Dedup-CaseInsensitive $inputServiceCandidates

    Add-DeviceBindingCheck `
        "device_binding_storage" `
        "Device Binding: Storage (virtio-blk)" `
        $devices `
        $storageServiceCandidates `
        @("SCSIAdapter","HDC") `
        $blkRegex `
        @("virtio","aero") `
        "No virtio-blk storage devices detected (system may still be using AHCI)."

    Add-DeviceBindingCheck `
        "device_binding_network" `
        "Device Binding: Network (virtio-net)" `
        $devices `
        $networkServiceCandidates `
        @("NET") `
        $netRegex `
        @("virtio","aero") `
        "No virtio-net devices detected (system may still be using e1000/baseline networking)."

    Add-DeviceBindingCheck `
        "device_binding_graphics" `
        "Device Binding: Graphics (Aero GPU / virtio-gpu)" `
        $devices `
        $graphicsServiceCandidates `
        @("DISPLAY") `
        $gpuRegex `
        @("aero","virtio","gpu") `
        "No Aero/virtio GPU devices detected (system may still be using VGA/baseline graphics)."

    # --- AeroGPU configuration (registry) ---
    # Surface the segment-budget override if present:
    #   HKR\Parameters\NonLocalMemorySizeMB (REG_DWORD)
    # This is informational: missing value implies the driver default.
    try {
        $gpuDevice = $null
        if ($report.checks.ContainsKey("device_binding_graphics")) {
            $chk = $report.checks["device_binding_graphics"]
            if ($chk -and $chk.data -and ($chk.data -is [hashtable]) -and $chk.data.matched_devices) {
                $md = $chk.data.matched_devices
                if ($md -and $md.Count -gt 0) { $gpuDevice = $md[0] }
            }
        }

        if (-not $report.aerogpu -or -not ($report.aerogpu -is [hashtable])) {
            $report.aerogpu = @{
                detected = $false
                pnp_device_id = $null
                adapter_registry_key = $null
                non_local_memory_size_mb = $null
                non_local_memory_size_mb_note = $null
                non_local_memory_size_mb_registry_path = $null
            }
        }

        if (-not $gpuDevice) {
            $report.aerogpu.detected = $false
            $report.aerogpu.non_local_memory_size_mb_note = "No AeroGPU device detected."
        } else {
            $report.aerogpu.detected = $true
            $pnpid = "" + $gpuDevice.pnp_device_id
            $report.aerogpu.pnp_device_id = $pnpid

            $reg = Resolve-DisplayAdapterRegistryKey $pnpid
            $report.aerogpu.adapter_registry_key = $reg

            $value = $null
            $valuePath = $null
            $resolvedAny = $false

            # Prefer the Display class key, but check Control\Video as a fallback.
            if ($reg -and $reg.display_class_key_exists -and $reg.display_class_key_path) {
                $resolvedAny = $true
                $paramKey = Join-Path $reg.display_class_key_path "Parameters"
                $v = Get-RegistryDword $paramKey "NonLocalMemorySizeMB"
                if ($v -ne $null) { $value = $v; $valuePath = $paramKey }
            }
            if ($value -eq $null -and $reg -and $reg.control_video_key_exists -and $reg.control_video_key_path) {
                $resolvedAny = $true
                $paramKey = Join-Path $reg.control_video_key_path "Parameters"
                $v = Get-RegistryDword $paramKey "NonLocalMemorySizeMB"
                if ($v -ne $null) { $value = $v; $valuePath = $paramKey }
            }

            $report.aerogpu.non_local_memory_size_mb = $value
            $report.aerogpu.non_local_memory_size_mb_registry_path = $valuePath

            if (-not $resolvedAny) {
                $report.aerogpu.non_local_memory_size_mb_note = "Unable to resolve AeroGPU adapter registry key; cannot query NonLocalMemorySizeMB."
            } elseif ($value -eq $null) {
                $report.aerogpu.non_local_memory_size_mb_note = "Not set (driver default)."
            } else {
                $report.aerogpu.non_local_memory_size_mb_note = "Configured registry override (HKR\\Parameters\\NonLocalMemorySizeMB)."
            }

            # Surface in report.txt under the existing graphics binding check.
            if ($report.checks.ContainsKey("device_binding_graphics")) {
                $chk = $report.checks["device_binding_graphics"]
                if (-not $chk.details) { $chk.details = @() }

                if ($reg -and $reg.resolved_key_path) {
                    $chk.details += ("Adapter registry key: " + $reg.resolved_key_path + " (" + $reg.resolved_key_kind + ")")
                } else {
                    $chk.details += "Adapter registry key: <unresolved>"
                }

                if (-not $resolvedAny) {
                    $chk.details += "NonLocalMemorySizeMB: <unknown> (unable to locate adapter registry key)"
                } elseif ($value -eq $null) {
                    $chk.details += "NonLocalMemorySizeMB: not set (driver default)"
                } else {
                    $chk.details += ("NonLocalMemorySizeMB: " + $value + " MB (override)")
                }
            }
        }
    } catch {
        # Informational only (do not affect overall PASS/WARN/FAIL).
        if ($report.aerogpu -and ($report.aerogpu -is [hashtable])) {
            $report.aerogpu.non_local_memory_size_mb_note = "Failed to query NonLocalMemorySizeMB: " + $_.Exception.Message
        }
    }

    # --- AeroGPU UMD DLL placement (WOW64 completeness) ---
    try {
        $gpuDetected = $false
        foreach ($d in $devices) {
            $pnpid = "" + $d.pnp_device_id
            if ($pnpid -and $gpuRegex -and ($pnpid -match $gpuRegex)) {
                $gpuDetected = $true
                break
            }
        }

        $umdStatus = "PASS"
        $umdSummary = ""
        $umdDetails = @()
        $umdData = @{
            gpu_detected = $gpuDetected
            is_64bit = $null
            expected_files = @()
            missing_files = @()
        }

        if (-not $gpuDetected) {
            # Avoid a redundant WARN: missing GPU is already surfaced by device_binding_graphics.
            $umdSummary = "Skipped: no AeroGPU device detected."
        } else {
            $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
            $umdData.is_64bit = $is64

            $expected = @()
            if ($is64) {
                $expected += (Join-Path (Join-Path $env:SystemRoot "System32") "aerogpu_d3d9_x64.dll")
                $expected += (Join-Path (Join-Path $env:SystemRoot "SysWOW64") "aerogpu_d3d9.dll")
            } else {
                $expected += (Join-Path (Join-Path $env:SystemRoot "System32") "aerogpu_d3d9.dll")
            }
            $umdData.expected_files = $expected

            $missing = @()
            foreach ($p in $expected) {
                if (-not (Test-Path $p)) { $missing += $p }
            }
            $umdData.missing_files = $missing

            if ($missing.Count -gt 0) {
                $umdStatus = "WARN"
                $umdSummary = "Missing expected AeroGPU D3D9 UMD DLL(s) (" + $missing.Count + "/" + $expected.Count + ")."
                $umdDetails += "Expected D3D9 UMD file(s):"
                foreach ($p in $expected) { $umdDetails += ("  - " + $p) }
                $umdDetails += "Missing:"
                foreach ($p in $missing) { $umdDetails += ("  - " + $p) }
                if ($is64) {
                    $umdDetails += "See: docs/windows7-driver-troubleshooting.md#issue-32-bit-d3d9-apps-fail-on-windows-7-x64-missing-wow64-umd"
                }
            } else {
                $umdSummary = "AeroGPU D3D9 UMD DLL(s) are present."
            }
        }

        Add-Check "aerogpu_umd_files" "AeroGPU D3D9 UMD DLL placement" $umdStatus $umdSummary $umdData $umdDetails
    } catch {
        Add-Check "aerogpu_umd_files" "AeroGPU D3D9 UMD DLL placement" "WARN" ("Failed: " + $_.Exception.Message) $null @()
    }

    # --- AeroGPU D3D10/11 UMD DLL placement (optional) ---
    # Only required if the DX11-capable driver package is installed.
    try {
        $gpuDetected = $false
        foreach ($d in $devices) {
            $pnpid = "" + $d.pnp_device_id
            if ($pnpid -and $gpuRegex -and ($pnpid -match $gpuRegex)) {
                $gpuDetected = $true
                break
            }
        }

        $dxStatus = "PASS"
        $dxSummary = ""
        $dxDetails = @()
        $dxData = @{
            gpu_detected = $gpuDetected
            is_64bit = $null
            expected_files = @()
            present_files = @()
            missing_files = @()
        }

        if (-not $gpuDetected) {
            # Avoid a redundant WARN: missing GPU is already surfaced by device_binding_graphics.
            $dxSummary = "Skipped: no AeroGPU device detected."
        } else {
            $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
            $dxData.is_64bit = $is64

            $expected = @()
            if ($is64) {
                $expected += (Join-Path (Join-Path $env:SystemRoot "System32") "aerogpu_d3d10_x64.dll")
                $expected += (Join-Path (Join-Path $env:SystemRoot "SysWOW64") "aerogpu_d3d10.dll")
            } else {
                $expected += (Join-Path (Join-Path $env:SystemRoot "System32") "aerogpu_d3d10.dll")
            }
            $dxData.expected_files = $expected

            $present = @()
            $missing = @()
            foreach ($p in $expected) {
                if (Test-Path $p) { $present += $p } else { $missing += $p }
            }
            $dxData.present_files = $present
            $dxData.missing_files = $missing

            if ($present.Count -eq 0) {
                $dxSummary = "Skipped: AeroGPU D3D10/11 UMD DLL(s) not detected (D3D10/11 is optional)."
            } elseif ($missing.Count -gt 0) {
                $dxStatus = "WARN"
                $dxSummary = "Missing expected AeroGPU D3D10/11 UMD DLL(s) (" + $missing.Count + "/" + $expected.Count + ")."
                $dxDetails += "Expected D3D10/11 UMD file(s):"
                foreach ($p in $expected) { $dxDetails += ("  - " + $p) }
                $dxDetails += "Missing:"
                foreach ($p in $missing) { $dxDetails += ("  - " + $p) }
                if ($is64) {
                    $dxDetails += "WOW64 D3D10/11 UMD is required for 32-bit D3D10/D3D11 apps on Win7 x64."
                    $dxDetails += "See: docs/windows7-driver-troubleshooting.md#issue-32-bit-d3d11-apps-fail-on-windows-7-x64-missing-wow64-d3d1011-umd"
                }
            } else {
                $dxSummary = "AeroGPU D3D10/11 UMD DLL(s) are present."
            }
        }

        Add-Check "aerogpu_d3d10_umd_files" "AeroGPU D3D10/11 UMD DLL placement" $dxStatus $dxSummary $dxData $dxDetails
    } catch {
        Add-Check "aerogpu_d3d10_umd_files" "AeroGPU D3D10/11 UMD DLL placement" "WARN" ("Failed: " + $_.Exception.Message) $null @()
    }

    Add-DeviceBindingCheck `
        "device_binding_audio" `
        "Device Binding: Audio (virtio-snd)" `
        $devices `
        $audioServiceCandidates `
        @("MEDIA") `
        $sndRegex `
        @("aero","virtio","audio") `
        "No virtio audio devices detected."

    Add-DeviceBindingCheck `
        "device_binding_input" `
        "Device Binding: Input (virtio-input)" `
        $devices `
        $inputServiceCandidates `
        @("HIDClass","Keyboard","Mouse") `
        $inputRegex `
        @("aero","virtio","input") `
        "No virtio input devices detected (system may still be using PS/2 input)."
} catch {
    Add-Check "bound_devices" "Bound Devices (WMI Win32_PnPEntity)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Optional packaged tools (tools\*) ---
try {
    $toolsDir = Join-Path $scriptDir "tools"
    $toolsStatus = "PASS"
    $toolsSummary = ""
    $toolsDetails = @()

    $toolsData = @{
        tools_dir = $toolsDir
        tools_dir_present = $false
        tools_dir_readable = $null
        files = @()
        exe_files = @()
        total_size_bytes = 0
        total_exe_size_bytes = 0
        extension_stats = @()
        manifest_present = $null
        manifest_parse_ok = $null
        manifest_includes_tools = $null
        manifest_tools_files_listed = $null
        manifest_listed_files_present = $null
        manifest_unlisted_files_present = $null
        inventory_errors = @()
    }

    $toolsDirItem = $null
    $toolsDirErr = $null
    try {
        $toolsDirItem = Get-Item -LiteralPath $toolsDir -ErrorAction Stop
    } catch {
        $isNotFound = $false
        try {
            if ($_.CategoryInfo -and ($_.CategoryInfo.Category -eq "ObjectNotFound")) { $isNotFound = $true }
        } catch { }
        try {
            if ($_.Exception -is [System.Management.Automation.ItemNotFoundException]) { $isNotFound = $true }
        } catch { }
        try {
            if ($_.Exception -is [System.IO.DirectoryNotFoundException]) { $isNotFound = $true }
        } catch { }
        try {
            if ($_.Exception -is [System.IO.FileNotFoundException]) { $isNotFound = $true }
        } catch { }
        try {
            $fid = "" + $_.FullyQualifiedErrorId
            if ($fid -match '(?i)PathNotFound') { $isNotFound = $true }
        } catch { }

        if ($isNotFound) {
            $toolsDirItem = $null
        } else {
            # Treat any other error as: directory may exist but is unreadable.
            $toolsDirItem = $true
            $toolsDirErr = $_.Exception.Message
        }
    }

    if (-not $toolsDirItem) {
        $toolsSummary = "No tools\\ directory present (optional)."
        Add-Check "optional_tools" "Optional Tools (tools\\*)" $toolsStatus $toolsSummary $toolsData $toolsDetails
    } else {
        $toolsData.tools_dir_present = $true

        if ($toolsDirErr) {
            $toolsStatus = "WARN"
            $toolsData.tools_dir_readable = $false
            $toolsSummary = "tools\\ directory is present but unreadable."
            $toolsDetails += ("Failed to access tools\\: " + $toolsDirErr)
            $toolsData.inventory_errors = @($toolsDirErr)
            Add-Check "optional_tools" "Optional Tools (tools\\*)" $toolsStatus $toolsSummary $toolsData $toolsDetails
        } elseif ($toolsDirItem -and ($toolsDirItem -isnot [bool]) -and (-not $toolsDirItem.PSIsContainer)) {
            $toolsStatus = "WARN"
            $toolsData.tools_dir_readable = $false
            $toolsSummary = "tools\\ exists but is not a directory."
            $toolsDetails += ("tools\\ path is not a directory: " + ("" + $toolsDirItem.FullName))
            $toolsData.inventory_errors = @("tools\\ exists but is not a directory")
            Add-Check "optional_tools" "Optional Tools (tools\\*)" $toolsStatus $toolsSummary $toolsData $toolsDetails
        } else {
            $gciErrors = @()
            $items = @()
            try {
                $items = Get-ChildItem -Path $toolsDir -Recurse -Force -ErrorAction SilentlyContinue -ErrorVariable gciErrors
            } catch {
                # Get-ChildItem can still throw in some cases; treat as unreadable.
                $gciErrors += $_
            }

            $invErrors = @()
            if ($gciErrors -and $gciErrors.Count -gt 0) {
                $toolsStatus = "WARN"
                foreach ($e in $gciErrors) {
                    try {
                        if ($e -and $e.Exception -and $e.Exception.Message) {
                            $invErrors += ("" + $e.Exception.Message)
                        } elseif ($e) {
                            $invErrors += ("" + $e.ToString())
                        }
                    } catch { }
                }
            }

            $rootFull = $null
            try { $rootFull = [System.IO.Path]::GetFullPath($scriptDir) } catch { $rootFull = $scriptDir }
            # Use a trailing path separator when computing relative paths to avoid prefix collisions
            # (e.g. C:\Foo vs C:\Foobar).
            $prefix = $rootFull
            if (-not ($prefix.EndsWith("\") -or $prefix.EndsWith("/"))) { $prefix += "\" }
            $prefixLower = ""
            try { $prefixLower = $prefix.ToLower() } catch { $prefixLower = "" }

            # Best-effort: correlate tools\ files against manifest.json entries (when present).
            $manifestFileResults = @{}
            $manifestPresent = $null
            $manifestParseOk = $null
            $manifestIncludesTools = $null
            $manifestToolsFilesListed = $null
            $manifestCorrelationAvailable = $false
            try {
                if ($report.media_integrity -and ($report.media_integrity -is [hashtable])) {
                    if ($report.media_integrity.ContainsKey("manifest_present")) { $manifestPresent = $report.media_integrity.manifest_present }
                    if ($report.media_integrity.ContainsKey("parse_ok")) { $manifestParseOk = $report.media_integrity.parse_ok }
                    if ($report.media_integrity.ContainsKey("manifest_includes_tools")) { $manifestIncludesTools = $report.media_integrity.manifest_includes_tools }
                    if ($report.media_integrity.ContainsKey("tools_files_listed")) { $manifestToolsFilesListed = $report.media_integrity.tools_files_listed }
                    if ($report.media_integrity.ContainsKey("file_results") -and $report.media_integrity.file_results) {
                        foreach ($r in $report.media_integrity.file_results) {
                            if (-not $r -or -not $r.path) { continue }
                            $p = ("" + $r.path).Trim()
                            if ($p.Length -eq 0) { continue }
                            $p = $p.Replace("\", "/")
                            if ($p.StartsWith("./")) { $p = $p.Substring(2) }
                            while ($p.StartsWith("/")) { $p = $p.Substring(1) }
                            if ($p.Length -eq 0) { continue }
                            $manifestFileResults[$p.ToLower()] = $r
                        }
                    }
                }
            } catch { }
            if (($manifestParseOk -eq $true) -and ($manifestFileResults.Count -gt 0)) { $manifestCorrelationAvailable = $true }

            $fileItems = @()
            foreach ($it in @($items)) {
                try {
                    if ($it -and (-not $it.PSIsContainer)) { $fileItems += $it }
                } catch { }
            }

            $files = @()
            $exeFiles = @()
            $fileItemsSorted = @($fileItems | Sort-Object FullName)
            foreach ($f in @($fileItemsSorted)) {
                if (-not $f -or -not $f.FullName) { continue }
                $full = "" + $f.FullName
                $rel = $full
                try {
                    $fullCanon = [System.IO.Path]::GetFullPath($full)
                    if ($prefixLower -and $fullCanon.ToLower().StartsWith($prefixLower)) {
                        $rel = $fullCanon.Substring($prefix.Length)
                        $rel = $rel.TrimStart('\','/')
                    } else {
                        $rel = $fullCanon
                    }
                } catch { }

                $relNorm = $rel.Replace("\", "/")
                if ($relNorm.StartsWith("./")) { $relNorm = $relNorm.Substring(2) }
                while ($relNorm.StartsWith("/")) { $relNorm = $relNorm.Substring(1) }

                $listed = $null
                $manifestResult = $null
                if ($manifestCorrelationAvailable) {
                    $listed = $false
                    try {
                        $k = $relNorm.ToLower()
                        if ($manifestFileResults.ContainsKey($k)) {
                            $listed = $true
                            $manifestResult = $manifestFileResults[$k]
                        }
                    } catch { $listed = $false; $manifestResult = $null }
                }

                $sha = $null
                try {
                    if ($manifestResult -and ($manifestResult -is [hashtable]) -and $manifestResult.ContainsKey("actual_sha256") -and $manifestResult["actual_sha256"]) {
                        $sha = "" + $manifestResult["actual_sha256"]
                    }
                } catch { $sha = $null }
                if (-not $sha -or $sha.Length -eq 0) {
                $sha = Get-FileSha256Hex $full
                if (-not $sha) {
                    $toolsStatus = "WARN"
                    $invErrors += ("Failed to compute SHA-256 for: " + $rel)
                }
                }

                $isExe = $false
                $extLower = ""
                try {
                    $extLower = "" + [System.IO.Path]::GetExtension($full)
                    $extLower = $extLower.ToLower()
                    if ($extLower -eq ".exe") { $isExe = $true }
                } catch { $isExe = $false; $extLower = "" }

                # FileVersionInfo is relatively expensive; only gather it for PE-like files.
                $fileVersion = $null
                $productVersion = $null
                $fileDescription = $null
                $originalFilename = $null
                if ($extLower -eq ".exe" -or $extLower -eq ".dll") {
                    $vi = $null
                    try {
                        $vi = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($full)
                    } catch { $vi = $null }
                    if ($vi) {
                        try { if ($vi.FileVersion) { $fileVersion = "" + $vi.FileVersion } } catch { }
                        try { if ($vi.ProductVersion) { $productVersion = "" + $vi.ProductVersion } } catch { }
                        try { if ($vi.FileDescription) { $fileDescription = "" + $vi.FileDescription } } catch { }
                        try { if ($vi.OriginalFilename) { $originalFilename = "" + $vi.OriginalFilename } } catch { }
                    }
                }

                $entry = @{
                    relative_path = $rel
                    relative_path_normalized = $relNorm
                    listed_in_manifest = $listed
                    sha256 = $sha
                    size_bytes = $f.Length
                    file_version = $fileVersion
                    product_version = $productVersion
                    file_description = $fileDescription
                    original_filename = $originalFilename
                }
                $files += $entry
                if ($isExe) { $exeFiles += $entry }
            }
            $files = @($files | Sort-Object { $_.relative_path })
            $exeFiles = @($exeFiles | Sort-Object { $_.relative_path })

            $totalBytes = 0
            $totalExeBytes = 0
            foreach ($e in $files) {
                try {
                    if ($e -and ($e.size_bytes -ne $null)) { $totalBytes += [int64]$e.size_bytes }
                } catch { }
            }
            foreach ($e in $exeFiles) {
                try {
                    if ($e -and ($e.size_bytes -ne $null)) { $totalExeBytes += [int64]$e.size_bytes }
                } catch { }
            }

            $listedCount = $null
            $unlistedCount = $null
            if ($manifestCorrelationAvailable) {
                $listedCount = 0
                foreach ($e in $files) {
                    try {
                        if ($e -and ($e.listed_in_manifest -eq $true)) { $listedCount++ }
                    } catch { }
                }
                $unlistedCount = ($files.Count - $listedCount)
            }

            $extMap = @{}
            foreach ($e in $files) {
                try {
                    $p = "" + $e.relative_path_normalized
                    $ext = ""
                    try { $ext = "" + [System.IO.Path]::GetExtension($p) } catch { $ext = "" }
                    $ext = $ext.ToLower()
                    if (-not $ext -or $ext.Length -eq 0) { $ext = "<none>" }
                    if ($extMap.ContainsKey($ext)) {
                        $extMap[$ext] = ([int]$extMap[$ext]) + 1
                    } else {
                        $extMap[$ext] = 1
                    }
                } catch { }
            }
            $extStats = @()
            foreach ($k in $extMap.Keys) {
                $extStats += @{
                    extension = "" + $k
                    count = [int]$extMap[$k]
                }
            }
            # Deterministic ordering for report stability: count desc, then extension desc.
            $extStats = @($extStats | Sort-Object count, extension -Descending)

            $toolsData.files = $files
            $toolsData.exe_files = $exeFiles
            $toolsData.total_size_bytes = $totalBytes
            $toolsData.total_exe_size_bytes = $totalExeBytes
            $toolsData.extension_stats = $extStats
            $toolsData.manifest_present = $manifestPresent
            $toolsData.manifest_parse_ok = $manifestParseOk
            $toolsData.manifest_includes_tools = $manifestIncludesTools
            $toolsData.manifest_tools_files_listed = $manifestToolsFilesListed
            $toolsData.manifest_listed_files_present = $listedCount
            $toolsData.manifest_unlisted_files_present = $unlistedCount
            $toolsData.inventory_errors = $invErrors
            $toolsData.tools_dir_readable = ($toolsStatus -eq "PASS")

            $toolsSummary = "tools\\ directory present. File(s): " + $files.Count + "; EXE file(s): " + $exeFiles.Count
            if ($toolsStatus -eq "WARN") {
                $toolsSummary += " (inventory incomplete)."
            }
            $toolsSummary += "; total_bytes=" + $totalBytes
            if ($manifestCorrelationAvailable) {
                $toolsSummary += "; manifest_listed=" + $listedCount + ", unlisted=" + $unlistedCount
            }

            foreach ($e in $exeFiles) {
                $line = "" + $e.relative_path
                if ($e.sha256) { $line += " sha256=" + $e.sha256 } else { $line += " sha256=<error>" }
                if ($e.size_bytes -ne $null) { $line += " size=" + $e.size_bytes }
                if ($e.file_version) { $line += " filever=" + $e.file_version }
                $toolsDetails += $line
            }

            if ($extStats -and $extStats.Count -gt 0) {
                $parts = @()
                $shown = 0
                $maxParts = 10
                foreach ($p in $extStats) {
                    if ($shown -ge $maxParts) { break }
                    $parts += ("" + $p.extension + "=" + $p.count)
                    $shown++
                }
                $toolsDetails += ("Extensions: " + ($parts -join ", "))
                if ($extStats.Count -gt $maxParts) {
                    $toolsDetails += ("Extensions: ... and " + ($extStats.Count - $maxParts).ToString() + " more (see report.json)")
                }
            }

            $otherFiles = @()
            foreach ($f in $files) {
                try {
                    $rp = "" + $f.relative_path_normalized
                    $ext = ""
                    try { $ext = "" + [System.IO.Path]::GetExtension($rp) } catch { $ext = "" }
                    if ($ext -and ($ext.ToLower() -eq ".exe")) { continue }
                } catch { }
                $otherFiles += $f
            }

            if ($otherFiles.Count -gt 0) {
                $toolsDetails += ("Other (non-EXE) file(s): " + $otherFiles.Count)
                $maxList = 25
                $shown = 0
                foreach ($e in $otherFiles) {
                    if ($shown -ge $maxList) { break }
                    $line = "" + $e.relative_path
                    if ($e.sha256) {
                        $h = "" + $e.sha256
                        $hs = $h
                        if ($hs.Length -gt 12) { $hs = $hs.Substring(0, 12) }
                        $line += " sha256=" + $hs + "..."
                    }
                    if ($e.size_bytes -ne $null) { $line += " size=" + $e.size_bytes }
                    if ($e.file_version) { $line += " filever=" + $e.file_version }
                    $toolsDetails += $line
                    $shown++
                }
                if ($otherFiles.Count -gt $maxList) {
                    $toolsDetails += ("... and " + ($otherFiles.Count - $maxList).ToString() + " more (see report.json)")
                }
            }
            if ($manifestCorrelationAvailable) {
                $toolsDetails += ("Manifest correlation: listed_present=" + $listedCount + ", unlisted_present=" + $unlistedCount + ", manifest_includes_tools=" + $manifestIncludesTools + ", manifest_tools_files_listed=" + $manifestToolsFilesListed)
            }
            foreach ($m in $invErrors) {
                if ($m) { $toolsDetails += ("Inventory error: " + $m) }
            }

            Add-Check "optional_tools" "Optional Tools (tools\\*)" $toolsStatus $toolsSummary $toolsData $toolsDetails
        }
    }
} catch {
    Add-Check "optional_tools" "Optional Tools (tools\\*)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Installed driver binding correlation (media INFs vs active device bindings) ---
try {
    $media = $report.packaged_drivers_summary
    $bindingSummary = @{
        media_arch = (if ($media -and $media.ContainsKey("arch")) { $media.arch } else { $null })
        media_drivers_root = (if ($media -and $media.ContainsKey("drivers_root")) { $media.drivers_root } else { $null })
        hwid_index_size = 0
        analysed_devices = 0
        matched_media_driver = 0
        media_hwid_match_driver_mismatch = 0
        no_media_match = 0
        no_signed_driver = 0
        device_results = @()
    }

    $status = "PASS"
    $summaryText = ""
    $details = @()

    $index = @()
    if ($media -and $media.ContainsKey("driver_folders") -and $media.driver_folders) {
        foreach ($pkg in $media.driver_folders) {
            if (-not $pkg.infs) { continue }
            foreach ($inf in $pkg.infs) {
                if (-not $inf.hwid_prefixes) { continue }
                foreach ($hwid in $inf.hwid_prefixes) {
                    if (-not $hwid -or $hwid.Length -eq 0) { continue }
                    $index += @{
                        hwid_prefix = $hwid
                        hwid_prefix_upper = ("" + $hwid).ToUpper()
                        driver_folder = "" + $pkg.name
                        inf_rel_path = "" + $inf.inf_rel_path
                        provider = "" + $inf.provider
                        driver_ver_raw = "" + $inf.driver_ver_raw
                        driver_version = "" + $inf.driver_version
                        add_services = $inf.add_services
                    }
                }
            }
        }
    }
    $bindingSummary.hwid_index_size = $index.Count

    if (-not $index -or $index.Count -eq 0) {
        $status = "WARN"
        $summaryText = "No HWID patterns were parsed from packaged driver INFs; cannot correlate installed bindings to media."
        $details += "Verify you are running from a complete Guest Tools ISO/zip and that drivers\\<arch> contains .inf files."
    } elseif (-not $boundDevicesForCorrelation) {
        $status = "WARN"
        $summaryText = "No device inventory available from Win32_PnPEntity; cannot correlate installed bindings to media."
    } else {
        $bindingSummary.analysed_devices = $boundDevicesForCorrelation.Count

        foreach ($d in $boundDevicesForCorrelation) {
            $pnpid = "" + $d.pnp_device_id
            if (-not $pnpid -or $pnpid.Length -eq 0) { continue }
            $pnpUpper = $pnpid.ToUpper()

            $bestLen = -1
            $best = $null
            foreach ($e in $index) {
                if ($pnpUpper.StartsWith($e.hwid_prefix_upper)) {
                    $l = $e.hwid_prefix_upper.Length
                    if ($l -gt $bestLen) { $bestLen = $l; $best = $e }
                }
            }

            $sd = $d.signed_driver
            $installedProvider = $null
            $installedVersion = $null
            $installedInf = $null
            $installedSigner = $null
            if ($sd) {
                $installedProvider = "" + $sd.driver_provider_name
                $installedVersion = "" + $sd.driver_version
                $installedInf = "" + $sd.inf_name
                $installedSigner = "" + $sd.signer
            }

            $assessment = "NO_MEDIA_MATCH"
            if (-not $best) {
                $bindingSummary.no_media_match++
            } elseif (-not $sd) {
                $assessment = "MEDIA_HWID_MATCH_NO_SIGNED_DRIVER"
                $bindingSummary.no_signed_driver++
            } else {
                $providerMatch = $false
                $versionMatch = $false

                if ($best.provider -and $installedProvider -and ($best.provider.ToLower() -eq $installedProvider.ToLower())) { $providerMatch = $true }
                if (-not $best.provider -or -not $installedProvider) { $providerMatch = $true } # missing data: don't penalize

                if ($best.driver_version -and $installedVersion -and ($best.driver_version.Trim() -eq $installedVersion.Trim())) { $versionMatch = $true }
                if (-not $best.driver_version -or -not $installedVersion) { $versionMatch = $true }

                if ($providerMatch -and $versionMatch) {
                    $assessment = "MATCHED_MEDIA_DRIVER"
                    $bindingSummary.matched_media_driver++
                } else {
                    $assessment = "MEDIA_HWID_MATCH_DRIVER_MISMATCH"
                    $bindingSummary.media_hwid_match_driver_mismatch++
                }
            }

            $bindingSummary.device_results += @{
                name = "" + $d.name
                manufacturer = "" + $d.manufacturer
                pnp_device_id = $pnpid
                pnp_class = "" + $d.pnp_class
                service = "" + $d.service
                status = "" + $d.status
                config_manager_error_code = $d.config_manager_error_code
                config_manager_error_meaning = "" + $d.config_manager_error_meaning
                installed_driver = $sd
                media_match = (if ($best) {
                    @{
                        hwid_prefix = "" + $best.hwid_prefix
                        driver_folder = "" + $best.driver_folder
                        inf_rel_path = "" + $best.inf_rel_path
                        provider = "" + $best.provider
                        driver_ver_raw = "" + $best.driver_ver_raw
                        driver_version = "" + $best.driver_version
                        add_services = $best.add_services
                    }
                } else { $null })
                assessment = $assessment
            }

            $line = $assessment + ": " + $d.name
            $line += " (PNPDeviceID=" + $pnpid + ")"
            if ($installedInf) { $line += ", INF=" + $installedInf }
            if ($installedProvider) { $line += ", Provider=" + $installedProvider }
            if ($installedVersion) { $line += ", Version=" + $installedVersion }
            if ($installedSigner) { $line += ", Signer=" + $installedSigner }
            if ($best) { $line += " | MediaINF=" + $best.inf_rel_path }
            $details += $line
        }

        $summaryText = "Analysed " + $bindingSummary.analysed_devices + " relevant device(s): matched=" + $bindingSummary.matched_media_driver + ", mismatch=" + $bindingSummary.media_hwid_match_driver_mismatch + ", no_media_match=" + $bindingSummary.no_media_match + ", no_signed_driver=" + $bindingSummary.no_signed_driver

        if ($bindingSummary.analysed_devices -eq 0) {
            $status = "WARN"
            $details += "No virtio/Aero devices were detected. If you expected them, verify the VM hardware profile and run Device Manager -> Scan for hardware changes."
        } elseif ($bindingSummary.no_media_match -gt 0 -or $bindingSummary.media_hwid_match_driver_mismatch -gt 0 -or $bindingSummary.no_signed_driver -gt 0) {
            $status = "WARN"
            $details += "Remediation: If devices are not matching the media, re-run setup.cmd from the correct Guest Tools ISO/zip and avoid mixing driver folders across versions."
            $details += "See: docs/windows7-driver-troubleshooting.md#issue-virtio-device-not-found-or-unknown-device-after-switching"
        }
    }

    $report.installed_driver_binding_summary = $bindingSummary
    Add-Check "installed_driver_binding_summary" "Installed Driver Binding Correlation (media vs system)" $status $summaryText $bindingSummary $details
} catch {
    $report.installed_driver_binding_summary = @{
        error = $_.Exception.Message
    }
    Add-Check "installed_driver_binding_summary" "Installed Driver Binding Correlation (media vs system)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- virtio-blk storage service ---
try {
    $rerunHint = "Re-run setup.cmd"
    if ($storagePreseedSkipped) { $rerunHint = "Run setup.cmd again without /skipstorage once virtio-blk drivers are available" }
    $rerunHintSentence = $rerunHint + "."

    $candidates = @("viostor","aeroviostor","virtio_blk","virtio-blk","aerostor","aeroblk")
    if ($cfgVirtioBlkService) { $candidates = @($cfgVirtioBlkService) + $candidates }
    $candDedup = @()
    foreach ($c in $candidates) {
        $exists = $false
        foreach ($e in $candDedup) {
            if ($e.ToLower() -eq $c.ToLower()) { $exists = $true; break }
        }
        if (-not $exists) { $candDedup += $c }
    }
    $candidates = $candDedup

    $expected = $cfgVirtioBlkService
    if (-not $expected) { $expected = "viostor" }
    $found = $null
    foreach ($name in $candidates) {
        $drv = Try-GetWmi "Win32_SystemDriver" ("Name='" + $name.Replace("'","''") + "'")
        if ($drv) {
            $svcKey = "HKLM:\SYSTEM\CurrentControlSet\Services\" + $name
            $startValue = Get-RegistryDword $svcKey "Start"
            $imagePath = Get-RegistryString $svcKey "ImagePath"
            $group = Get-RegistryString $svcKey "Group"
            $type = Get-RegistryDword $svcKey "Type"
            $errorControl = Get-RegistryDword $svcKey "ErrorControl"
            $found = @{
                name = $drv.Name
                display_name = $drv.DisplayName
                state = $drv.State
                status = $drv.Status
                start_mode = $drv.StartMode
                path_name = $drv.PathName
                registry_start_value = $startValue
                registry_start_type = (if ($startValue -ne $null) { StartType-FromStartValue $startValue } else { $null })
                registry_image_path = $imagePath
                registry_group = $group
                registry_type = $type
                registry_error_control = $errorControl
            }
            break
        }
    }

    $svcStatus = "PASS"
    $svcSummary = ""
    $svcDetails = @()

    if ($storagePreseedSkipped) {
        $svcDetails += ("Storage pre-seeding was intentionally skipped by setup.cmd (/skipstorage). Do NOT switch the boot disk to virtio-blk until pre-seeding has been performed (marker: " + $storagePreseedSkipMarker + ").")
    }

    if (-not $found) {
        $svcStatus = "WARN"
        $svcSummary = "virtio-blk service not found (tried: " + ($candidates -join ", ") + ")."
        if ($storagePreseedSkipped) { $svcSummary += " NOTE: storage pre-seeding was skipped by setup.cmd (/skipstorage)." }
        $svcDetails += ("If Aero storage drivers are installed, expected a driver service like '" + $expected + "'.")
        $svcDetails += "See: docs/windows7-driver-troubleshooting.md#issue-storage-controller-switch-gotchas-boot-loops-0x7b"
    } else {
        $bootCriticalIssue = $false
        $expectedSys = $cfgVirtioBlkSys
        if (-not $expectedSys) { $expectedSys = $found.name + ".sys" }
        $driversDir = Join-Path (Join-Path $env:SystemRoot "System32") "drivers"
        $expectedSysPath = Join-Path $driversDir $expectedSys
        $expectedSysExists = Test-Path $expectedSysPath

        $resolvedImagePath = $null
        $resolvedImageExists = $null
        if ($found.registry_image_path) {
            $p = $found.registry_image_path.Trim()
            $p = $p.Trim('"')
            $p = [Environment]::ExpandEnvironmentVariables($p)
            if ($p.StartsWith("\??\")) { $p = $p.Substring(4) }

            # Common driver ImagePath formats:
            #   \SystemRoot\System32\drivers\foo.sys
            #   system32\drivers\foo.sys
            #   C:\Windows\System32\drivers\foo.sys
            if ($p.StartsWith("\SystemRoot", [StringComparison]::OrdinalIgnoreCase)) {
                $tail = $p.Substring(10) # length of "\SystemRoot"
                $tail = $tail.TrimStart('\')
                $p = Join-Path $env:SystemRoot $tail
            } elseif ($p.StartsWith("System32\", [StringComparison]::OrdinalIgnoreCase)) {
                $p = Join-Path $env:SystemRoot $p
            }

            $resolvedImagePath = $p
            $resolvedImageExists = Test-Path $resolvedImagePath
        }

        $found.expected_sys = $expectedSys
        $found.expected_sys_path = $expectedSysPath
        $found.expected_sys_exists = $expectedSysExists
        $found.resolved_image_path = $resolvedImagePath
        $found.resolved_image_exists = $resolvedImageExists

        $svcSummary = "Found service '" + $found.name + "': state=" + $found.state + ", start_mode=" + $found.start_mode
        if ($storagePreseedSkipped) { $svcSummary += " NOTE: storage pre-seeding was skipped by setup.cmd (/skipstorage)." }
        if ($found.registry_start_type) {
            $svcDetails += ("Registry Start=" + $found.registry_start_value + " (" + $found.registry_start_type + ")")
        }
        if ($found.registry_image_path) {
            $svcDetails += ("Registry ImagePath=" + $found.registry_image_path)
        }
        if ($found.registry_group) { $svcDetails += ("Registry Group=" + $found.registry_group) }
        if ($found.registry_type -ne $null) { $svcDetails += ("Registry Type=" + $found.registry_type) }
        if ($found.registry_error_control -ne $null) { $svcDetails += ("Registry ErrorControl=" + $found.registry_error_control) }
        if ($resolvedImagePath) {
            $svcDetails += ("Resolved ImagePath=" + $resolvedImagePath + " (exists=" + $resolvedImageExists + ")")
        }
        $svcDetails += ("Expected driver file=" + $expectedSysPath + " (exists=" + $expectedSysExists + ")")

        if (-not $found.registry_image_path) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += "Storage service ImagePath is missing. Boot loading may fail."
        } elseif ($resolvedImagePath -and ($resolvedImagePath.ToLower() -ne $expectedSysPath.ToLower())) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += ("Storage service ImagePath does not point to the expected driver file. Expected: " + $expectedSysPath + "; Resolved: " + $resolvedImagePath)
        }

        if (-not $expectedSysExists -and ($resolvedImageExists -ne $true)) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += ("Storage driver binary not found under System32\\drivers. Switching the boot disk to virtio-blk may fail (0x7B). " + $rerunHintSentence)
        }
        if ($found.registry_start_value -eq $null) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += ("Storage service Start value is missing/unreadable. Expected Start=0 (BOOT_START). Switching the boot disk to virtio-blk may fail (0x7B). " + $rerunHintSentence)
        } elseif ($found.registry_start_value -ne 0) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += ("Storage service is not configured as BOOT_START (Start=0). Switching the boot disk to virtio-blk may fail (0x7B). " + $rerunHintSentence)
        }
        if ($found.registry_type -ne $null -and $found.registry_type -ne 1) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += "Storage service Type is not 1 (kernel driver). Boot loading may fail."
        }
        if (-not $found.registry_group -or (("" + $found.registry_group).Trim().Length -eq 0)) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += "Storage service Group is missing/unreadable. Expected Group='SCSI miniport'. Boot loading order may be incorrect."
        } elseif ($found.registry_group.ToLower() -ne "scsi miniport") {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += "Storage service Group is not 'SCSI miniport'. Boot loading order may be incorrect."
        }
        if ($found.registry_error_control -ne $null -and $found.registry_error_control -ne 1) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $bootCriticalIssue = $true
            $svcDetails += "Storage service ErrorControl is not 1. Recommended is 1 for boot-critical storage."
        }
        if ($bootCriticalIssue) {
            $svcDetails += "See: docs/windows7-driver-troubleshooting.md#issue-storage-controller-switch-gotchas-boot-loops-0x7b"
        }
        if ($found.state -ne "Running") {
            $svcStatus = "WARN"
            $svcDetails += "Service is not running. A reboot may be required after driver install, or storage is not using virtio-blk."
        }
    }

    $svcData = @{
        candidates = $candidates
        found = $found
    }
    Add-Check "virtio_blk_service" "virtio-blk Storage Service" $svcStatus $svcSummary $svcData $svcDetails
} catch {
    Add-Check "virtio_blk_service" "virtio-blk Storage Service" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- virtio-blk boot-critical registry (CriticalDeviceDatabase) ---
try {
    $rerunHint = "Re-run setup.cmd"
    if ($storagePreseedSkipped) { $rerunHint = "Run setup.cmd again without /skipstorage once virtio-blk drivers are available" }
    $rerunHintSentence = $rerunHint + "."

    if (-not $cfgVirtioBlkService -or -not $cfgVirtioBlkHwids -or $cfgVirtioBlkHwids.Count -eq 0) {
        $data = @{
            config_loaded = $gtConfig.found
            config_file = $gtConfig.file_path
            config_service = $cfgVirtioBlkService
            configured_hwids = $cfgVirtioBlkHwids
        }
        Add-Check "virtio_blk_boot_critical" "virtio-blk Boot Critical Registry" "WARN" "Guest Tools config (config\\devices.cmd) is missing or incomplete; skipping CriticalDeviceDatabase verification." $data @()
    } else {
        $expectedService = $cfgVirtioBlkService
        $basePath = "HKLM:\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase"

        $expectedClass = "SCSIAdapter"
        $expectedClassGuid = "{4D36E97B-E325-11CE-BFC1-08002BE10318}"

        $variants = @(
            @{ suffix = ""; required = $true; kind = "hwid" },
            @{ suffix = "&CC_010000"; required = $false; kind = "compatible_id" },
            @{ suffix = "&CC_0100"; required = $false; kind = "compatible_id" }
        )

        $checkedKeys = @()
        $perHwid = @()

        $missingRequired = 0
        $missingOptional = 0
        $mismatchService = 0
        $mismatchClass = 0
        $mismatchGuid = 0

        foreach ($hwid in $cfgVirtioBlkHwids) {
            $baseKey = $hwid.Replace("\", "#")

            $hwidEntry = @{
                hwid = $hwid
                base_key = $baseKey
                variants = @()
                required_key_exists = $false
                required_key_service = $null
                required_key_service_matches = $null
            }

            foreach ($v in $variants) {
                $keyName = $baseKey + $v.suffix
                $path = Join-Path $basePath $keyName

                $exists = Test-Path $path
                $svc = $null
                $cls = $null
                $clsGuid = $null
                if ($exists) {
                    try {
                        $props = Get-ItemProperty -Path $path -ErrorAction Stop
                        $svc = $props.Service
                        $cls = $props.Class
                        $clsGuid = $props.ClassGUID
                    } catch {
                        $svc = $null
                        $cls = $null
                        $clsGuid = $null
                    }

                    if (-not $svc -or (("" + $svc).Trim().Length -eq 0)) {
                        $mismatchService++
                    } elseif ($svc.ToLower() -ne $expectedService.ToLower()) {
                        $mismatchService++
                    }
                    if ($cls -and ("" + $cls).ToLower() -ne $expectedClass.ToLower()) { $mismatchClass++ }
                    if ($clsGuid -and ("" + $clsGuid).ToLower() -ne $expectedClassGuid.ToLower()) { $mismatchGuid++ }
                } else {
                    if ($v.required) { $missingRequired++ } else { $missingOptional++ }
                }

                $checkedKeys += @{
                    key = $keyName
                    kind = $v.kind
                    required = $v.required
                    exists = $exists
                    service = $svc
                    expected_service = $expectedService
                    class = $cls
                    expected_class = $expectedClass
                    class_guid = $clsGuid
                    expected_class_guid = $expectedClassGuid
                }

                $hwidEntry.variants += @{
                    key = $keyName
                    kind = $v.kind
                    required = $v.required
                    exists = $exists
                    service = $svc
                }

                if ($v.required) {
                    $hwidEntry.required_key_exists = $exists
                    $hwidEntry.required_key_service = $svc
                    if ($svc -and ($svc.ToLower() -eq $expectedService.ToLower())) {
                        $hwidEntry.required_key_service_matches = $true
                    } elseif (-not $exists) {
                        $hwidEntry.required_key_service_matches = $null
                    } else {
                        $hwidEntry.required_key_service_matches = $false
                    }
                }
            }

            $perHwid += $hwidEntry
        }

        $status = "PASS"
        if ($missingRequired -gt 0 -or $mismatchService -gt 0 -or $mismatchClass -gt 0 -or $mismatchGuid -gt 0) {
            $status = "WARN"
        }
 
        $summary = "Checked CriticalDeviceDatabase for service '" + $expectedService + "' (HWIDs: " + $cfgVirtioBlkHwids.Count + "; missing_required=" + $missingRequired + ", missing_optional=" + $missingOptional + ", mismatched_service=" + $mismatchService + ", mismatched_class=" + $mismatchClass + ", mismatched_guid=" + $mismatchGuid + ")"
        if ($storagePreseedSkipped) { $summary += " NOTE: storage pre-seeding was skipped by setup.cmd (/skipstorage)." }

        $details = @()
        if ($storagePreseedSkipped) { $details += "Storage pre-seeding was intentionally skipped by setup.cmd (/skipstorage). Do NOT switch the boot disk to virtio-blk unless you later pre-seed storage (setup.cmd without /skipstorage) or manually configure the required keys." }
        if ($missingRequired -gt 0) {
            $details += "Missing CriticalDeviceDatabase keys for the configured virtio-blk HWIDs can cause 0x7B (INACCESSIBLE_BOOT_DEVICE) when switching the boot disk to virtio-blk."
            foreach ($h in $perHwid) {
                if (-not $h.required_key_exists) { $details += ("FAIL: Missing key: " + $h.base_key) }
            }
        }
        if ($mismatchService -gt 0) { $details += ("Some CriticalDeviceDatabase keys map to a different storage service than expected. " + $rerunHintSentence + " Ensure config\\devices.cmd matches the storage driver's INF AddService name.") }
        if ($missingOptional -gt 0) { $details += "Some compatible-ID keys (&CC_010000 / &CC_0100) are missing. Usually OK, but adding them improves early-boot matching coverage." }
        if ($mismatchClass -gt 0 -or $mismatchGuid -gt 0) { $details += ("Some keys have unexpected Class/ClassGUID. " + $rerunHint + " to regenerate CriticalDeviceDatabase entries.") }

        if ($status -ne "PASS") {
            $details += "See: docs/windows7-driver-troubleshooting.md#issue-storage-controller-switch-gotchas-boot-loops-0x7b"
        }

        $data = @{
            config_file = $gtConfig.file_path
            config_service = $expectedService
            configured_hwids = $cfgVirtioBlkHwids
            per_hwid = $perHwid
            checked_keys = $checkedKeys
            expected_class = $expectedClass
            expected_class_guid = $expectedClassGuid
            optional_suffixes = @("&CC_010000", "&CC_0100")
        }
        Add-Check "virtio_blk_boot_critical" "virtio-blk Boot Critical Registry" $status $summary $data $details
    }
} catch {
    Add-Check "virtio_blk_boot_critical" "virtio-blk Boot Critical Registry" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Smoke test: disk I/O ---
try {
    $testFile = Join-Path $outDir "io_smoke_test.bin"
    $data = [System.Text.Encoding]::UTF8.GetBytes(([System.Guid]::NewGuid().ToString() + "|" + (Get-Date).ToUniversalTime().ToString("o")))
    [System.IO.File]::WriteAllBytes($testFile, $data)
    $read = [System.IO.File]::ReadAllBytes($testFile)
    Remove-Item -Path $testFile -Force -ErrorAction SilentlyContinue

    $ok = ($read.Length -eq $data.Length)
    if ($ok) {
        for ($i = 0; $i -lt $data.Length; $i++) {
            if ($read[$i] -ne $data[$i]) { $ok = $false; break }
        }
    }

    if ($ok) {
        Add-Check "smoke_disk" "Smoke Test: Disk I/O" "PASS" "Create+read temporary file succeeded." @{ temp_path = $testFile } @()
    } else {
        Add-Check "smoke_disk" "Smoke Test: Disk I/O" "FAIL" "Create+read temporary file failed (data mismatch)." @{ temp_path = $testFile } @()
    }
} catch {
    Add-Check "smoke_disk" "Smoke Test: Disk I/O" "FAIL" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Smoke test: network ---
try {
    $configs = Try-GetWmi "Win32_NetworkAdapterConfiguration" "IPEnabled=TRUE"
    $adapterData = @()
    $gateways = @()
    if ($configs) {
        foreach ($cfg in $configs) {
            $idx = $cfg.Index
            $ad = Try-GetWmi "Win32_NetworkAdapter" ("Index=" + $idx)
            $gw = $null
            if ($cfg.DefaultIPGateway) {
                $gw = $cfg.DefaultIPGateway | Select-Object -First 1
                if ($gw) { $gateways += $gw }
            }
            $adapterData += @{
                index = $idx
                description = "" + $cfg.Description
                mac_address = "" + $cfg.MACAddress
                ip_address = $cfg.IPAddress
                default_gateway = $cfg.DefaultIPGateway
                dhcp_enabled = $cfg.DHCPEnabled
                net_connection_status = (if ($ad) { $ad.NetConnectionStatus } else { $null })
                net_enabled = (if ($ad) { $ad.NetEnabled } else { $null })
            }
        }
    }

    $connected = @()
    foreach ($a in $adapterData) {
        if ($a.net_connection_status -eq 2 -or $a.net_enabled -eq $true) { $connected += $a }
    }

    $target = $PingTarget
    if (-not $target -or $target.Length -eq 0) {
        if ($gateways.Count -gt 0) {
            $target = $gateways | Select-Object -First 1
        }
    }

    $pingResult = $null
    if ($target -and $target.Length -gt 0) {
        $ping = Invoke-Capture "ping.exe" @("-n","1","-w","1000",$target)
        $pingResult = @{
            target = $target
            exit_code = $ping.exit_code
            raw = $ping.output
            success = ($ping.exit_code -eq 0)
        }
    }

    $netStatus = "PASS"
    $netSummary = ""
    $netDetails = @()

    if (-not $adapterData -or $adapterData.Count -eq 0) {
        $netStatus = "WARN"
        $netSummary = "No IP-enabled network adapters detected."
    } else {
        $netSummary = "Adapters IP-enabled: " + $adapterData.Count + "; connected: " + $connected.Count
        if ($connected.Count -eq 0) {
            $netStatus = "WARN"
            $netDetails += "No connected adapters detected (link may be down)."
        }
    }

    if ($pingResult) {
        if ($pingResult.success) {
            $netDetails += ("Ping " + $pingResult.target + ": PASS")
        } else {
            $netStatus = Merge-Status $netStatus "WARN"
            $netDetails += ("Ping " + $pingResult.target + ": WARN (failed)")
        }
    } else {
        $netDetails += "Ping: skipped (no target and no default gateway detected)."
    }

    $netData = @{
        adapters = $adapterData
        default_gateways = $gateways
        ping = $pingResult
    }
    Add-Check "smoke_network" "Smoke Test: Network" $netStatus $netSummary $netData $netDetails
} catch {
    Add-Check "smoke_network" "Smoke Test: Network" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Smoke test: graphics ---
try {
    $vc = Try-GetWmi "Win32_VideoController" ""
    $controllers = @()
    if ($vc) {
        foreach ($v in $vc) {
            $controllers += @{
                name = "" + $v.Name
                status = "" + $v.Status
                driver_version = "" + $v.DriverVersion
                driver_date = "" + $v.DriverDate
                video_processor = "" + $v.VideoProcessor
                adapter_ram = $v.AdapterRAM
                current_horizontal_resolution = $v.CurrentHorizontalResolution
                current_vertical_resolution = $v.CurrentVerticalResolution
                current_refresh_rate = $v.CurrentRefreshRate
            }
        }
    }

    $gfxStatus = "PASS"
    $gfxSummary = ""
    $gfxDetails = @()

    if (-not $controllers -or $controllers.Count -eq 0) {
        $gfxStatus = "WARN"
        $gfxSummary = "No Win32_VideoController entries detected."
    } else {
        $okCount = @($controllers | Where-Object { $_.status -eq "OK" }).Count
        $gfxSummary = "Video controllers detected: " + $controllers.Count + " (Status=OK: " + $okCount + ")"
        if ($okCount -eq 0) {
            $gfxStatus = "WARN"
            $gfxDetails += "No video controller reports Status=OK."
        }

        foreach ($c in $controllers) {
            $line = "" + $c.name
            if ($c.current_horizontal_resolution -and $c.current_vertical_resolution) {
                $line += " (" + $c.current_horizontal_resolution + "x" + $c.current_vertical_resolution + ")"
            }
            if ($c.driver_version) { $line += ", DriverVersion=" + $c.driver_version }
            if ($c.status) { $line += ", Status=" + $c.status }
            $gfxDetails += $line
        }
    }

    Add-Check "smoke_graphics" "Smoke Test: Graphics" $gfxStatus $gfxSummary @{ video_controllers = $controllers } $gfxDetails
} catch {
    Add-Check "smoke_graphics" "Smoke Test: Graphics" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- AeroGPU dbgctl (best-effort diagnostics) ---
try {
    $dbgStatus = "PASS"
    $summary = ""
    $details = @()
    $dbgctlEnabled = ($RunDbgctl -or $RunDbgctlSelftest)

    # Template: drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe
    $dbgctlRelTemplate = 'drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe'

    $data = @{
        enabled = $dbgctlEnabled
        enabled_selftest = $RunDbgctlSelftest
        aerogpu_detected = $false
        aerogpu_healthy = $false
        aerogpu_config_manager_error_codes = @()
        expected_path_template = $dbgctlRelTemplate
        expected_path = $null
        found = $false
        path = $null
        searched = @()
        # Always run a safe, fast command when dbgctl exists.
        args = @("--help")
        fallback_args = @("/?")
        host_timeout_ms = 5000
        exit_code = $null
        stdout = $null
        stderr = $null
        timed_out = $false
        output_path = $null
        attempts = @()

        # Optional extended diagnostic run (only if -RunDbgctl is set and AeroGPU is healthy).
        status_args = @("--status","--timeout-ms","2000")
        status_tool_timeout_ms = 2000
        status_exit_code = $null
        status_stdout = $null
        status_stderr = $null
        status_timed_out = $false
        status_output_path = $null
    }

    # Resolve expected path early so it is always present in report.json surfaces
    # (even when dbgctl is skipped).
    $is64 = $false
    if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
        $is64 = ("" + $report.checks.os.data.architecture) -match '64'
    } else {
        $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
    }

    $arch = (if ($is64) { "amd64" } else { "x86" })
    $rel = $dbgctlRelTemplate.Replace("<arch>", $arch)
    $expectedPath = Join-Path $scriptDir $rel
    $data.expected_path = $expectedPath

    # Detect AeroGPU presence via the device binding check (A3A0:0001).
    $aeroOnlyRx = '(?i)^PCI\\(?:VEN|VID)_A3A0&(?:DEV|DID)_0001'
    $matched = $null
    try {
        if ($report -and $report.checks -and $report.checks.ContainsKey("device_binding_graphics")) {
            $chk = $report.checks["device_binding_graphics"]
            if ($chk -and $chk.data -and $chk.data.matched_devices) {
                $matched = $chk.data.matched_devices
            }
        }
    } catch { $matched = $null }

    if ($matched) {
        foreach ($d in $matched) {
            $pnpid = "" + $d.pnp_device_id
            if ($pnpid -and ($pnpid -match $aeroOnlyRx)) {
                $data.aerogpu_detected = $true
                $err = $d.config_manager_error_code
                if ($err -ne $null) { $data.aerogpu_config_manager_error_codes += $err }
                if ($err -eq 0) {
                    $data.aerogpu_healthy = $true
                    break
                }
            }
        }
    }

    # Prefer the canonical packaged location, then fall back to a broader search.
    if (Test-Path $expectedPath) {
        $data.found = $true
        $data.path = $expectedPath
        $data.searched = @($expectedPath)
    } else {
        $dbgctlInfo = Find-AeroGpuDbgctl $scriptDir $is64
        $data.found = $dbgctlInfo.found
        $data.path = $dbgctlInfo.path
        $data.searched = $dbgctlInfo.searched
    }

    if (-not $data.found -or -not $data.path) {
        if ($dbgctlEnabled) {
            $dbgStatus = "WARN"
            $summary = "aerogpu_dbgctl.exe not found on Guest Tools media; skipping dbgctl diagnostics."
            $details += "Expected: " + $expectedPath
        } else {
            $summary = "aerogpu_dbgctl.exe not found on Guest Tools media (optional)."
        }
    } else {
        # Always run a safe/bounded command for basic provenance (version/help).
        $attempts = @()
        $cap = Invoke-CaptureWithTimeout $data.path $data.args $data.host_timeout_ms
        $attempts += @{
            args = $data.args
            exit_code = $cap.exit_code
            stdout = $cap.stdout
            stderr = $cap.stderr
            timed_out = $cap.timed_out
        }

        $combined = ("" + $cap.stdout + $cap.stderr)
        if ($cap.timed_out -or ($cap.exit_code -ne 0) -or (-not $combined) -or $combined.Trim().Length -eq 0) {
            $cap2 = Invoke-CaptureWithTimeout $data.path $data.fallback_args $data.host_timeout_ms
            $attempts += @{
                args = $data.fallback_args
                exit_code = $cap2.exit_code
                stdout = $cap2.stdout
                stderr = $cap2.stderr
                timed_out = $cap2.timed_out
            }
        }

        # Choose the "best" attempt: prefer non-timeout, exit_code=0, and some output.
        $best = $null
        foreach ($a in $attempts) {
            $outText = ("" + $a.stdout + $a.stderr)
            $hasOut = ($outText -and $outText.Trim().Length -gt 0)
            if (($a.timed_out -ne $true) -and ($a.exit_code -eq 0) -and $hasOut) { $best = $a; break }
        }
        if (-not $best) {
            foreach ($a in $attempts) {
                $outText = ("" + $a.stdout + $a.stderr)
                $hasOut = ($outText -and $outText.Trim().Length -gt 0)
                if (($a.timed_out -ne $true) -and $hasOut) { $best = $a; break }
            }
        }
        if (-not $best) { $best = $attempts[0] }

        $data.attempts = $attempts
        $data.args = $best.args
        $data.exit_code = $best.exit_code
        $data.stdout = $best.stdout
        $data.stderr = $best.stderr
        $data.timed_out = $best.timed_out

        if ($best.timed_out -or ($best.exit_code -ne 0)) {
            $dbgStatus = "WARN"
        }

        $summary = "aerogpu_dbgctl " + ($best.args -join " ") + " exit_code=" + (if ($best.exit_code -ne $null) { $best.exit_code } else { "null" })
        if ($best.timed_out) { $summary += " (timed out)" }

        $details += "Tool: " + $data.path
        $details += "Args: " + ($best.args -join " ")
        if ($best.exit_code -ne $null) { $details += "Exit code: " + $best.exit_code }
        if ($best.timed_out) { $details += ("Timed out: true (host timeout " + $data.host_timeout_ms + " ms)") }

        # Save output as a convenience artifact for bug reports.
        $versionFile = Join-Path $outDir "dbgctl_version.txt"
        try {
            $toWrite = $best.stdout
            if (-not $toWrite) { $toWrite = "" }
            if ($best.stderr) { $toWrite += "`r`n--- STDERR ---`r`n" + $best.stderr }
            Set-Content -Path $versionFile -Value $toWrite -Encoding UTF8
            $data.output_path = $versionFile
            $details += "Saved: " + $versionFile
        } catch { }

        $excerpt = Get-TextExcerpt (($best.stdout + "`r`n" + $best.stderr)) 30 8000
        if ($excerpt -and $excerpt.Trim().Length -gt 0) {
            $details += "Output excerpt:"
            foreach ($line in ($excerpt -split "`r?`n")) {
                if ($line -eq $null) { continue }
                $t = ("" + $line).TrimEnd()
                if ($t.Length -eq 0) { continue }
                $details += ("  " + $t)
            }
        }

        # Optional extended status run, only if requested and AeroGPU is present+healthy.
        if ($RunDbgctl) {
            if (-not $data.aerogpu_detected) {
                $details += "Extended dbgctl: skipped --status (no AeroGPU device detected)."
            } elseif (-not $data.aerogpu_healthy) {
                $codes = @($data.aerogpu_config_manager_error_codes | Sort-Object -Unique)
                $details += "Extended dbgctl: skipped --status (AeroGPU device not healthy; ConfigManagerErrorCode != 0)."
                if ($codes -and $codes.Count -gt 0) { $details += ("AeroGPU CM codes: " + ($codes -join ",")) }
            } else {
                $capS = Invoke-CaptureWithTimeout $data.path $data.status_args $data.host_timeout_ms
                $data.status_exit_code = $capS.exit_code
                $data.status_stdout = $capS.stdout
                $data.status_stderr = $capS.stderr
                $data.status_timed_out = $capS.timed_out

                if ($capS.timed_out -or ($capS.exit_code -ne 0)) {
                    $dbgStatus = Merge-Status $dbgStatus "WARN"
                }

                $details += "Extended dbgctl: ran --status"
                $details += "Status args: " + ($data.status_args -join " ")
                if ($capS.exit_code -ne $null) { $details += "Status exit code: " + $capS.exit_code }
                if ($capS.timed_out) { $details += ("Status timed out: true (host timeout " + $data.host_timeout_ms + " ms)") }

                $statusFile = Join-Path $outDir "dbgctl_status.txt"
                try {
                    $toWrite = $capS.stdout
                    if (-not $toWrite) { $toWrite = "" }
                    if ($capS.stderr) { $toWrite += "`r`n--- STDERR ---`r`n" + $capS.stderr }
                    Set-Content -Path $statusFile -Value $toWrite -Encoding UTF8
                    $data.status_output_path = $statusFile
                    $details += "Saved: " + $statusFile
                } catch { }
            }
        }
    }

    # Ensure a stable JSON surface for bug reports: aerogpu.dbgctl.
    if (-not $report.aerogpu) { $report.aerogpu = @{} }
    $report.aerogpu.dbgctl = @{
        expected_path_template = $data.expected_path_template
        expected_path = $data.expected_path
        path = $data.path
        args = $data.args
        exit_code = $data.exit_code
        stdout = $data.stdout
        stderr = $data.stderr
        timed_out = $data.timed_out
        output_path = $data.output_path
        status_args = $data.status_args
        status_exit_code = $data.status_exit_code
        status_stdout = $data.status_stdout
        status_stderr = $data.status_stderr
        status_timed_out = $data.status_timed_out
        status_output_path = $data.status_output_path
    }

    Add-Check "aerogpu_dbgctl" "AeroGPU dbgctl (optional diagnostics)" $dbgStatus $summary $data $details
} catch {
    Add-Check "aerogpu_dbgctl" "AeroGPU dbgctl (optional diagnostics)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- AeroGPU dbgctl selftest (optional in-guest diagnostics) ---
try {
    $dbgStatus = "PASS"
    $summary = ""
    $details = @()

    $data = @{
        enabled = $RunDbgctlSelftest
        expected_path_template = 'drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe'
        expected_path = $null
        found = $false
        path = $null
        args = @("--selftest","--timeout-ms","2000")
        tool_timeout_ms = 2000
        host_timeout_ms = 8000
        exit_code = $null
        stdout = $null
        stderr = $null
        timed_out = $false
        output_path = $null
        aerogpu_detected = $false
        aerogpu_healthy = $false
        aerogpu_config_manager_error_codes = @()
    }

    # Reuse dbgctl path + AeroGPU health detection from the previous check (avoids duplicating the device search logic).
    $base = $null
    try {
        if ($report -and $report.checks -and $report.checks.ContainsKey("aerogpu_dbgctl")) {
            $base = $report.checks["aerogpu_dbgctl"].data
        }
    } catch { $base = $null }

    if ($base) {
        if ($base.expected_path_template) { $data.expected_path_template = $base.expected_path_template }
        if ($base.expected_path) { $data.expected_path = $base.expected_path }
        if ($base.found -ne $null) { $data.found = $base.found }
        if ($base.path) { $data.path = $base.path }
        if ($base.aerogpu_detected -ne $null) { $data.aerogpu_detected = $base.aerogpu_detected }
        if ($base.aerogpu_healthy -ne $null) { $data.aerogpu_healthy = $base.aerogpu_healthy }
        if ($base.aerogpu_config_manager_error_codes) { $data.aerogpu_config_manager_error_codes = $base.aerogpu_config_manager_error_codes }
    }

    if (-not $RunDbgctlSelftest) {
        $summary = "Skipped: -RunDbgctlSelftest not set."
    } elseif (-not $data.aerogpu_detected) {
        $summary = "Skipped: no AeroGPU device detected."
    } elseif (-not $data.aerogpu_healthy) {
        $codes = @($data.aerogpu_config_manager_error_codes | Sort-Object -Unique)
        $summary = "Skipped: AeroGPU device detected but not healthy (ConfigManagerErrorCode != 0)."
        if ($codes -and $codes.Count -gt 0) { $summary += " CM=" + ($codes -join ",") }
    } elseif (-not $data.found -or -not $data.path) {
        $dbgStatus = "WARN"
        $summary = "aerogpu_dbgctl.exe not found on Guest Tools media; skipping selftest."
        if ($data.expected_path) { $details += "Expected: " + $data.expected_path }
    } else {
        $cap = Invoke-CaptureWithTimeout $data.path $data.args $data.host_timeout_ms
        $data.exit_code = $cap.exit_code
        $data.stdout = $cap.stdout
        $data.stderr = $cap.stderr
        $data.timed_out = $cap.timed_out

        if ($cap.timed_out -or ($cap.exit_code -ne 0)) {
            $dbgStatus = "WARN"
        }

        $summary = "aerogpu_dbgctl --selftest exit_code=" + (if ($cap.exit_code -ne $null) { $cap.exit_code } else { "null" })
        if ($cap.timed_out) { $summary += " (timed out)" }

        $details += "Tool: " + $data.path
        $details += "Args: " + ($data.args -join " ")
        if ($cap.exit_code -ne $null) { $details += "Exit code: " + $cap.exit_code }
        if ($cap.timed_out) { $details += ("Timed out: true (host timeout " + $data.host_timeout_ms + " ms)") }

        # Save output as a convenience artifact for bug reports.
        $outFile = Join-Path $outDir "dbgctl_selftest.txt"
        try {
            $toWrite = $cap.stdout
            if (-not $toWrite) { $toWrite = "" }
            if ($cap.stderr) { $toWrite += "`r`n--- STDERR ---`r`n" + $cap.stderr }
            Set-Content -Path $outFile -Value $toWrite -Encoding UTF8
            $data.output_path = $outFile
            $details += "Saved: " + $outFile
        } catch { }

        if ($cap.stdout) {
            $details += "Stdout:"
            foreach ($line in ($cap.stdout -split "`r?`n")) {
                if ($line -eq $null) { continue }
                $t = ("" + $line).TrimEnd()
                if ($t.Length -eq 0) { continue }
                $details += ("  " + $t)
            }
        }
        if ($cap.stderr) {
            $details += "Stderr:"
            foreach ($line in ($cap.stderr -split "`r?`n")) {
                if ($line -eq $null) { continue }
                $t = ("" + $line).TrimEnd()
                if ($t.Length -eq 0) { continue }
                $details += ("  " + $t)
            }
        }
    }

    # Ensure a stable JSON surface for bug reports: aerogpu.dbgctl_selftest.
    if (-not $report.aerogpu) { $report.aerogpu = @{} }
    $report.aerogpu.dbgctl_selftest = @{
        expected_path_template = $data.expected_path_template
        expected_path = $data.expected_path
        path = $data.path
        exit_code = $data.exit_code
        stdout = $data.stdout
        stderr = $data.stderr
    }

    Add-Check "aerogpu_dbgctl_selftest" "AeroGPU dbgctl selftest (optional diagnostics)" $dbgStatus $summary $data $details
} catch {
    Add-Check "aerogpu_dbgctl_selftest" "AeroGPU dbgctl selftest (optional diagnostics)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Smoke test: audio ---
try {
    $sound = Try-GetWmi "Win32_SoundDevice" ""
    $devs = @()
    if ($sound) {
        foreach ($s in $sound) {
            $devs += @{
                name = "" + $s.Name
                manufacturer = "" + $s.Manufacturer
                status = "" + $s.Status
                pnp_device_id = "" + $s.PNPDeviceID
            }
        }
    }

    $audioStatus = "PASS"
    $audioSummary = ""
    $audioDetails = @()

    if (-not $devs -or $devs.Count -eq 0) {
        $audioStatus = "WARN"
        $audioSummary = "No Win32_SoundDevice entries detected."
    } else {
        $okCount = @($devs | Where-Object { $_.status -eq "OK" }).Count
        $audioSummary = "Sound devices detected: " + $devs.Count + " (Status=OK: " + $okCount + ")"
        if ($okCount -eq 0) {
            $audioStatus = "WARN"
            $audioDetails += "No sound device reports Status=OK."
        }
    }

    $playData = $null
    if ($PlayTestSound) {
        $mediaDir = Join-Path $env:WINDIR "Media"
        $wav = $null
        if (Test-Path $mediaDir) {
            $wav = Get-ChildItem -Path $mediaDir -Filter *.wav -ErrorAction SilentlyContinue | Select-Object -First 1
        }

        if ($wav) {
            try {
                $player = New-Object System.Media.SoundPlayer($wav.FullName)
                $player.Load()
                $player.PlaySync()
                $playData = @{ attempted = $true; wav_path = $wav.FullName; status = "PASS" }
                $audioDetails += ("PlayTestSound: PASS (" + $wav.FullName + ")")
            } catch {
                $audioStatus = Merge-Status $audioStatus "WARN"
                $playData = @{ attempted = $true; wav_path = $wav.FullName; status = "WARN"; error = $_.Exception.Message }
                $audioDetails += ("PlayTestSound: WARN (" + $_.Exception.Message + ")")
            }
        } else {
            $audioStatus = Merge-Status $audioStatus "WARN"
            $playData = @{ attempted = $true; status = "WARN"; error = "No .wav found under " + $mediaDir }
            $audioDetails += "PlayTestSound: WARN (no system .wav found)"
        }
    } else {
        $playData = @{ attempted = $false }
    }

    $audioData = @{
        sound_devices = $devs
        play_test_sound = $playData
    }
    Add-Check "smoke_audio" "Smoke Test: Audio" $audioStatus $audioSummary $audioData $audioDetails
} catch {
    Add-Check "smoke_audio" "Smoke Test: Audio" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Smoke test: input ---
try {
    $kb = Try-GetWmi "Win32_Keyboard" ""
    $mice = Try-GetWmi "Win32_PointingDevice" ""
    $kbData = @()
    $mouseData = @()

    if ($kb) {
        foreach ($k in $kb) {
            $kbData += @{
                name = "" + $k.Name
                description = "" + $k.Description
                status = "" + $k.Status
            }
        }
    }
    if ($mice) {
        foreach ($m in $mice) {
            $mouseData += @{
                name = "" + $m.Name
                description = "" + $m.Description
                status = "" + $m.Status
            }
        }
    }

    $inputStatus = "PASS"
    $inputSummary = "Keyboards: " + $kbData.Count + "; pointing devices: " + $mouseData.Count
    $inputDetails = @()

    if ($kbData.Count -eq 0) {
        $inputStatus = Merge-Status $inputStatus "WARN"
        $inputDetails += "No keyboards detected (Win32_Keyboard empty)."
    }
    if ($mouseData.Count -eq 0) {
        $inputStatus = Merge-Status $inputStatus "WARN"
        $inputDetails += "No pointing devices detected (Win32_PointingDevice empty)."
    }

    $inputData = @{
        keyboards = $kbData
        pointing_devices = $mouseData
    }
    Add-Check "smoke_input" "Smoke Test: Input" $inputStatus $inputSummary $inputData $inputDetails
} catch {
    Add-Check "smoke_input" "Smoke Test: Input" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

$ended = Get-Date
$report.tool.ended_utc = $ended.ToUniversalTime().ToString("o")
$report.tool.duration_ms = [int]([TimeSpan]($ended - $started)).TotalMilliseconds

switch ($report.overall.status) {
    "PASS" { $report.overall.summary = "All checks passed." }
    "WARN" { $report.overall.summary = "One or more checks reported WARN. Review report.txt for details." }
    "FAIL" { $report.overall.summary = "One or more checks failed. Review report.txt for details." }
}

# Write reports (best-effort).
try {
    Write-TextReport $report $txtPath
} catch {
    # If text report fails, we still want a JSON with an error.
    $report.errors += ("Failed to write " + $txtPath + ": " + $_.Exception.Message)
}
try {
    $json = ConvertTo-JsonCompat $report
    $jsonPretty = Format-Json $json
    Set-Content -Path $jsonPath -Value $jsonPretty -Encoding UTF8
} catch {
    $report.errors += ("Failed to write " + $jsonPath + ": " + $_.Exception.Message)
}

# Exit code: 0=PASS, 1=WARN, 2=FAIL
if ($report.overall.status -eq "PASS") { exit 0 }
if ($report.overall.status -eq "WARN") { exit 1 }
exit 2
