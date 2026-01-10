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
        Add-Check $key $title "WARN" $missingSummary $data $details
        return
    }

    $ok = @($matches | Where-Object { $_.config_manager_error_code -eq 0 }).Count
    $bad = @($matches | Where-Object { $_.config_manager_error_code -ne $null -and $_.config_manager_error_code -ne 0 }).Count

    $status = "PASS"
    $summary = "Matched devices: " + $matches.Count + " (OK: " + $ok + ", Problem: " + $bad + ")"
    if ($bad -gt 0 -and $ok -gt 0) { $status = "WARN" }
    if ($bad -gt 0 -and $ok -eq 0) { $status = "FAIL" }

    Add-Check $key $title $status $summary $data $details
}

function Load-CertFromFile([string]$path) {
    try {
        return New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($path)
    } catch {
        return $null
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

    foreach ($key in @("os","kb3033929","certificate_store","signature_mode","driver_packages","bound_devices","device_binding_storage","device_binding_network","device_binding_graphics","device_binding_audio","device_binding_input","virtio_blk_service","smoke_disk","smoke_network","smoke_audio","smoke_input")) {
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
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_BLK_SERVICE")) { $cfgVirtioBlkService = $cfgVars["AERO_VIRTIO_BLK_SERVICE"] }

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

$outDir = "C:\AeroGuestTools"
$jsonPath = Join-Path $outDir "report.json"
$txtPath = Join-Path $outDir "report.txt"

$report = @{
    tool = @{
        name = "Aero Guest Tools Verify"
        version = "1.2.0"
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

# --- Hotfix: KB3033929 (SHA-256 signature support) ---
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

    $is64 = $false
    if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
        $is64 = ("" + $report.checks.os.data.architecture) -match '64'
    } else {
        $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
    }

    $kbStatus = "PASS"
    $kbSummary = ""
    $kbDetails = @()

    if ($installed) {
        $kbSummary = "KB3033929 is installed."
    } else {
        $kbSummary = "KB3033929 is NOT installed."
        if ($is64) {
            $kbStatus = "WARN"
            $kbDetails += "Windows 7 x64 may require KB3033929 to validate SHA-256-signed driver catalogs (otherwise Device Manager Code 52)."
        } else {
            $kbDetails += "If your driver packages are SHA-256 signed, KB3033929 may still be required."
        }
    }

    Add-Check "kb3033929" "Hotfix: KB3033929 (SHA-256 signatures)" $kbStatus $kbSummary $kbInfo $kbDetails
} catch {
    Add-Check "kb3033929" "Hotfix: KB3033929 (SHA-256 signatures)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Certificate store (driver signing trust) ---
try {
    $certSearchDirs = @($scriptDir)
    $certDir = Join-Path $scriptDir "certs"
    if (Test-Path $certDir) { $certSearchDirs += $certDir }

    $certFiles = @()
    foreach ($dir in $certSearchDirs) {
        $certFiles += (Get-ChildItem -Path $dir -Filter *.cer -ErrorAction SilentlyContinue)
        $certFiles += (Get-ChildItem -Path $dir -Filter *.crt -ErrorAction SilentlyContinue)
    }

    $certResults = @()
    foreach ($cf in $certFiles) {
        $cert = Load-CertFromFile $cf.FullName
        if (-not $cert) {
            $certResults += @{
                file = $cf.Name
                status = "WARN"
                error = "Unable to load certificate file."
            }
            continue
        }

        $thumb = "" + $cert.Thumbprint
        $subj = "" + $cert.Subject
        $rootLM = Find-CertInStore $thumb "Root" "LocalMachine"
        $pubLM = Find-CertInStore $thumb "TrustedPublisher" "LocalMachine"

        $certResults += @{
            file = $cf.Name
            thumbprint = $thumb
            subject = $subj
            not_after = $cert.NotAfter.ToUniversalTime().ToString("o")
            local_machine_root = $rootLM
            local_machine_trusted_publisher = $pubLM
        }
    }

    $certStatus = "PASS"
    $certSummary = ""
    $certDetails = @()

    if (-not $certFiles -or $certFiles.Count -eq 0) {
        $certSummary = "No certificate files found under Guest Tools root/certs; skipping certificate store verification."
    } else {
        $missing = @()
        foreach ($cr in $certResults) {
            if ($cr.status -eq "WARN") { $missing += $cr; continue }
            if (-not $cr.local_machine_root -or -not $cr.local_machine_trusted_publisher) { $missing += $cr }
        }

        $certSummary = "Certificate file(s) found: " + $certFiles.Count
        if ($missing.Count -gt 0) {
            $certStatus = "WARN"
            $certDetails += ($missing.Count.ToString() + " certificate(s) are not installed in both LocalMachine Root + TrustedPublisher stores.")
            $certDetails += "Re-run Guest Tools setup as Administrator to install the driver certificate."
        }
    }

    $certData = @{
        script_dir = $scriptDir
        search_dirs = $certSearchDirs
        cert_files = @($certFiles | ForEach-Object { $_.Name })
        certificates = $certResults
    }
    Add-Check "certificate_store" "Certificate Store (driver signing trust)" $certStatus $certSummary $certData $certDetails
} catch {
    Add-Check "certificate_store" "Certificate Store (driver signing trust)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
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
        $is64 = $false
        if ($report.checks.ContainsKey("os") -and $report.checks.os.data -and $report.checks.os.data.architecture) {
            $is64 = ("" + $report.checks.os.data.architecture) -match '64'
        } else {
            $is64 = ("" + $env:PROCESSOR_ARCHITECTURE) -match '64'
        }

        if ($nicOn) {
            $sigStatus = "WARN"
            $sigDetails += "nointegritychecks is enabled. This is not recommended; prefer testsigning or properly signed drivers."
        }
        if (-not $tsOn) {
            if ($is64) {
                $sigStatus = Merge-Status $sigStatus "WARN"
            }
            $sigDetails += "testsigning is not enabled. If Aero drivers are test-signed (common on Windows 7 x64), enable it: bcdedit /set testsigning on"
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

        $isAero = $false
        $lower = $b.ToLower()
        foreach ($kw in $keywords) {
            if ($lower.Contains($kw)) { $isAero = $true; break }
        }

        $packages += @{
            published_name = $published
            is_aero_related = $isAero
            class = $class
            provider = $provider
            driver_date_and_version = $driverDateAndVersion
            signer_name = $signerName
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
    if ($cfgVirtioBlkService) { $svcCandidates = @($cfgVirtioBlkService) + $svcCandidates }
    $svcDedup = @()
    foreach ($s in $svcCandidates) {
        $exists = $false
        foreach ($e in $svcDedup) {
            if ($e.ToLower() -eq $s.ToLower()) { $exists = $true; break }
        }
        if (-not $exists) { $svcDedup += $s }
    }
    $svcCandidates = $svcDedup
    $kw = @("aero","virtio","1af4")

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
        }
        $code28 = @($devices | Where-Object { $_.config_manager_error_code -eq 28 })
        if ($code28.Count -gt 0) {
            $devStatus = Merge-Status $devStatus "WARN"
            $devDetails += ($code28.Count.ToString() + " device(s) report Code 28 (drivers not installed). Re-run Guest Tools setup / update driver in Device Manager.")
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

    # Per-device-class binding checks (best-effort).
    # These are intentionally WARN (not FAIL) when missing, since the guest might still be
    # using baseline devices (AHCI/e1000/VGA/PS2) even if Guest Tools are installed.

    $hwidVendorFallback = '(?i)(VEN_1AF4|VID_1AF4)'
    $blkRegex = $cfgVirtioBlkRegex
    $netRegex = $cfgVirtioNetRegex
    $sndRegex = $cfgVirtioSndRegex
    $inputRegex = $cfgVirtioInputRegex
    $gpuRegex = $cfgGpuRegex
    if (-not $blkRegex) { $blkRegex = $hwidVendorFallback }
    if (-not $netRegex) { $netRegex = $hwidVendorFallback }
    if (-not $sndRegex) { $sndRegex = $hwidVendorFallback }
    if (-not $inputRegex) { $inputRegex = $hwidVendorFallback }
    if (-not $gpuRegex) { $gpuRegex = '(?i)(VEN_1AE0|VID_1AE0|VEN_1AF4|VID_1AF4)' }

    $storageServiceCandidates = @("viostor","aeroviostor","virtio_blk","virtio-blk","aerostor","aeroblk")
    if ($cfgVirtioBlkService) { $storageServiceCandidates = @($cfgVirtioBlkService) + $storageServiceCandidates }

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
        @("vionet","netkvm") `
        @("NET") `
        $netRegex `
        @("virtio","aero") `
        "No virtio-net devices detected (system may still be using e1000/baseline networking)."

    Add-DeviceBindingCheck `
        "device_binding_graphics" `
        "Device Binding: Graphics (Aero GPU / virtio-gpu)" `
        $devices `
        @("viogpu","aerogpu","aero-gpu") `
        @("DISPLAY") `
        $gpuRegex `
        @("aero","virtio","gpu") `
        "No Aero/virtio GPU devices detected (system may still be using VGA/baseline graphics)."

    Add-DeviceBindingCheck `
        "device_binding_audio" `
        "Device Binding: Audio (virtio-snd)" `
        $devices `
        @("viosnd","aerosnd") `
        @("MEDIA") `
        $sndRegex `
        @("aero","virtio","audio") `
        "No virtio audio devices detected."

    Add-DeviceBindingCheck `
        "device_binding_input" `
        "Device Binding: Input (virtio-input)" `
        $devices `
        @("vioinput","aeroinput") `
        @("HIDClass","Keyboard","Mouse") `
        $inputRegex `
        @("aero","virtio","input") `
        "No virtio input devices detected (system may still be using PS/2 input)."
} catch {
    Add-Check "bound_devices" "Bound Devices (WMI Win32_PnPEntity)" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- virtio-blk storage service ---
try {
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
        $svcDetails += ("If Aero storage drivers are installed, expected a driver service like '" + $expected + "'.")
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
