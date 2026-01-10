param(
    # Optional: IP/hostname to ping for the network smoke test.
    # If omitted, the script will ping the default gateway (if present).
    [string]$PingTarget = "",

    # Optional: attempt to play a system .wav using System.Media.SoundPlayer.
    [switch]$PlayTestSound
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

    foreach ($key in @("os","signature_mode","driver_packages","bound_devices","virtio_blk_service","smoke_disk","smoke_network","smoke_audio","smoke_input")) {
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
$outDir = "C:\AeroGuestTools"
$jsonPath = Join-Path $outDir "report.json"
$txtPath = Join-Path $outDir "report.txt"

$report = @{
    tool = @{
        name = "Aero Guest Tools Verify"
        version = "1.0.0"
        started_utc = $started.ToUniversalTime().ToString("o")
        ended_utc = $null
        duration_ms = $null
        script_path = $MyInvocation.MyCommand.Path
        command_line = $MyInvocation.Line
        output_dir = $outDir
        report_json_path = $jsonPath
        report_txt_path = $txtPath
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

# --- Signature mode (bcdedit) ---
try {
    $bcd = Invoke-Capture "bcdedit.exe" @("/enum")
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
        if (-not $testsigning) { $testsigning = "Unknown/NotSet" }
        if (-not $nointegritychecks) { $nointegritychecks = "Unknown/NotSet" }
        $sigSummary = "testsigning=" + $testsigning + ", nointegritychecks=" + $nointegritychecks

        # Interpret values loosely (locales vary).
        $tsOn = ($testsigning -match '^(?i)(yes|on|true|1)$')
        $nicOn = ($nointegritychecks -match '^(?i)(yes|on|true|1)$')

        if ($nicOn) {
            $sigStatus = "WARN"
            $sigDetails += "nointegritychecks is enabled. This is not recommended; prefer testsigning or properly signed drivers."
        }
        if (-not $tsOn) {
            $sigStatus = Merge-Status $sigStatus "WARN"
            $sigDetails += "testsigning is not enabled. If Aero drivers are test-signed, enable it: bcdedit /set testsigning on"
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

    $keywords = @("aero","virtio","viostor","vionet","netkvm","viogpu","vioinput","viosnd","1af4")
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

        $isAero = $false
        $lower = $b.ToLower()
        foreach ($kw in $keywords) {
            if ($lower.Contains($kw)) { $isAero = $true; break }
        }

        $packages += @{
            published_name = $published
            is_aero_related = $isAero
            raw_block = $b
        }
    }

    $aeroPackages = @($packages | Where-Object { $_.is_aero_related })

    $drvData = @{
        pnputil_exit_code = $pnp.exit_code
        pnputil_raw = $raw
        total_packages_parsed = $packages.Count
        aero_packages = $aeroPackages
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
        $drvSummary = "Detected " + $aeroPackages.Count + " Aero-related driver package(s) (parsed " + $packages.Count + " total)."
        if ($aeroPackages.Count -eq 0) {
            $drvStatus = "WARN"
            $drvDetails += "No Aero-related packages matched heuristic keywords. See pnputil_raw in report.json."
        }
    }
    Add-Check "driver_packages" "Driver Packages (pnputil -e)" $drvStatus $drvSummary $drvData $drvDetails
} catch {
    Add-Check "driver_packages" "Driver Packages (pnputil -e)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Bound devices (WMI Win32_PnPEntity; optional devcon) ---
try {
    $devconDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $devconPath = Join-Path $devconDir "devcon.exe"

    $svcCandidates = @("viostor","aeroviostor","virtio_blk","virtio-blk","vionet","netkvm","viogpu","viosnd","vioinput")
    $kw = @("aero","virtio","1af4")

    $devices = @()
    $pnp = Try-GetWmi "Win32_PnPEntity" ""
    if ($pnp) {
        foreach ($d in $pnp) {
            $name = "" + $d.Name
            $mfr = "" + $d.Manufacturer
            $pnpid = "" + $d.PNPDeviceID
            $svc = "" + $d.Service

            $relevant = $false
            if ($pnpid -match '(?i)(VEN_1AF4|VID_1AF4|VIRTIO|AERO)') { $relevant = $true }
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
                $devices += @{
                    name = $name
                    manufacturer = $mfr
                    pnp_device_id = $pnpid
                    service = $svc
                    status = "" + $d.Status
                    config_manager_error_code = $d.ConfigManagerErrorCode
                    class_guid = "" + $d.ClassGuid
                }
            }
        }
    }

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
    }

    $devData = @{
        devices = $devices
        used_devcon = (Test-Path $devconPath)
        devcon_path = (if (Test-Path $devconPath) { $devconPath } else { $null })
        devcon_exit_code = (if ($devcon) { $devcon.exit_code } else { $null })
        devcon_raw = (if ($devcon) { $devcon.output } else { $null })
    }
    Add-Check "bound_devices" "Bound Devices (WMI Win32_PnPEntity)" $devStatus $devSummary $devData $devDetails
} catch {
    Add-Check "bound_devices" "Bound Devices (WMI Win32_PnPEntity)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- virtio-blk storage service ---
try {
    $candidates = @("viostor","aeroviostor","virtio_blk","virtio-blk","aerostor","aeroblk")
    $found = $null
    foreach ($name in $candidates) {
        $drv = Try-GetWmi "Win32_SystemDriver" ("Name='" + $name.Replace("'","''") + "'")
        if ($drv) {
            $startValue = Get-RegistryDword ("HKLM:\SYSTEM\CurrentControlSet\Services\" + $name) "Start"
            $found = @{
                name = $drv.Name
                display_name = $drv.DisplayName
                state = $drv.State
                status = $drv.Status
                start_mode = $drv.StartMode
                path_name = $drv.PathName
                registry_start_value = $startValue
                registry_start_type = (if ($startValue -ne $null) { StartType-FromStartValue $startValue } else { $null })
            }
            break
        }
    }

    $svcStatus = "PASS"
    $svcSummary = ""
    $svcDetails = @()

    if (-not $found) {
        $svcStatus = "WARN"
        $svcSummary = "virtio-blk service not found (tried: " + ($candidates -join ", ") + ")."
        $svcDetails += "If Aero storage drivers are installed, expected a driver service like 'viostor'."
    } else {
        $svcSummary = "Found service '" + $found.name + "': state=" + $found.state + ", start_mode=" + $found.start_mode
        if ($found.registry_start_type) {
            $svcDetails += ("Registry Start=" + $found.registry_start_value + " (" + $found.registry_start_type + ")")
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
