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
        "guest_tools_setup_state",
        "guest_tools_config",
        "kb3033929",
        "certificate_store",
        "signature_mode",
        "driver_packages",
        "bound_devices",
        "device_binding_storage",
        "device_binding_network",
        "device_binding_graphics",
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
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_BLK_SERVICE")) { $cfgVirtioBlkService = $cfgVars["AERO_VIRTIO_BLK_SERVICE"] }
$cfgVirtioBlkSys = $null
if ($cfgVars -and $cfgVars.ContainsKey("AERO_VIRTIO_BLK_SYS")) {
    $cfgVirtioBlkSys = ("" + $cfgVars["AERO_VIRTIO_BLK_SYS"]).Trim()
    if ($cfgVirtioBlkSys.Length -eq 0) { $cfgVirtioBlkSys = $null }
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

$outDir = "C:\AeroGuestTools"
$jsonPath = Join-Path $outDir "report.json"
$txtPath = Join-Path $outDir "report.txt"

$report = @{
    schema_version = 1
    tool = @{
        name = "Aero Guest Tools Verify"
        version = "2.0.1"
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

# --- Guest Tools manifest (version/build provenance) ---
try {
    $manifestPath = Join-Path $scriptDir "manifest.json"
    $manifestData = $null
    $mStatus = "PASS"
    $mSummary = ""
    $mDetails = @()

    if (-not (Test-Path $manifestPath)) {
        $mStatus = "WARN"
        $mSummary = "manifest.json not found next to verify.ps1; Guest Tools build metadata unavailable."
    } else {
        $raw = Get-Content -Path $manifestPath -ErrorAction Stop | Out-String
        $parsed = Parse-JsonCompat $raw
        if (-not $parsed) {
            $mStatus = "WARN"
            $mSummary = "manifest.json exists but could not be parsed."
        } else {
            $manifestData = @{
                path = $manifestPath
                version = (if ($parsed.ContainsKey("version")) { $parsed["version"] } else { $null })
                build_id = (if ($parsed.ContainsKey("build_id")) { $parsed["build_id"] } else { $null })
                source_date_epoch = (if ($parsed.ContainsKey("source_date_epoch")) { $parsed["source_date_epoch"] } else { $null })
            }
            $mSummary = "Guest Tools: version=" + $manifestData.version + ", build_id=" + $manifestData.build_id
        }
    }

    Add-Check "guest_tools_manifest" "Guest Tools Manifest" $mStatus $mSummary $manifestData $mDetails
} catch {
    Add-Check "guest_tools_manifest" "Guest Tools Manifest" "WARN" ("Failed: " + $_.Exception.Message) $null @()
}

# --- Guest Tools setup state (C:\AeroGuestTools\*) ---
$gtInstalledDriverPackages = @()
try {
    $installLog = Join-Path $outDir "install.log"
    $pkgList = Join-Path $outDir "installed-driver-packages.txt"
    $certList = Join-Path $outDir "installed-certs.txt"
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

    $st = "PASS"
    $sum = ""
    $det = @()

    $hasAny = (Test-Path $installLog) -or (Test-Path $pkgList) -or (Test-Path $certList)
    if (-not $hasAny) {
        $st = "WARN"
        $sum = "No Guest Tools setup state files found under " + $outDir + " (setup.cmd may not have been run yet)."
    } else {
        $sum = "install.log=" + (Test-Path $installLog) + ", installed-driver-packages=" + $gtInstalledDriverPackages.Count + ", installed-certs=" + $installedCertThumbprints.Count
        if (Test-Path $stateTestSign) { $det += "TestSigning was enabled by setup.cmd (marker file present)." }
        if (Test-Path $stateNoIntegrity) { $det += "nointegritychecks was enabled by setup.cmd (marker file present)." }
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
        }
        if ($cfgVirtioBlkService) { $cfgDetails += "AERO_VIRTIO_BLK_SERVICE=" + $cfgVirtioBlkService }
        if ($cfgVirtioBlkSys) { $cfgDetails += "AERO_VIRTIO_BLK_SYS=" + $cfgVirtioBlkSys }
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
        $certFiles += (Get-ChildItem -Path $dir -Filter *.p7b -ErrorAction SilentlyContinue)
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

    if (-not $certFiles -or $certFiles.Count -eq 0) {
        $certSummary = "No certificate files found under Guest Tools root/certs; skipping certificate store verification."
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

        $certSummary = "Certificate file(s) found: " + $certFiles.Count + "; certificates parsed: " + (@($certResults | Where-Object { $_.thumbprint }).Count)
        if ($badCount -gt 0 -or $missingCount -gt 0) {
            $certStatus = "WARN"
            if ($badCount -gt 0) { $certDetails += ($badCount.ToString() + " certificate file(s) could not be parsed.") }
            if ($missingCount -gt 0) { $certDetails += ($missingCount.ToString() + " certificate(s) are not installed in both LocalMachine Root + TrustedPublisher stores.") }
            $certDetails += "Re-run Guest Tools setup as Administrator to install the driver certificate(s)."
        }
    }

    $certData = @{
        script_dir = $scriptDir
        search_dirs = $certSearchDirs
        cert_files = @($certFiles | ForEach-Object { $_.FullName })
        certificates = $certResults
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

    $drvData = @{
        pnputil_exit_code = $pnp.exit_code
        pnputil_raw = $raw
        total_packages_parsed = $packages.Count
        aero_packages = $aeroPackages
        aero_packages_installed_by_guest_tools = $aeroInstalledByGt
        guest_tools_installed_driver_packages = $gtInstalledDriverPackages
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
        $drvSummary = "Detected " + $aeroPackages.Count + " Aero-related driver package(s) (installed-by-GuestTools: " + $aeroInstalledByGt.Count + "; parsed " + $packages.Count + " total)."
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
    $kw = @("aero","virtio","1af4","1ae0")

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
            if ($pnpid -match '(?i)(VEN_1AF4|VID_1AF4|VEN_1AE0|VID_1AE0|VIRTIO|AERO)') { $relevant = $true }
            if (-not $relevant -and $pnpid) {
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
            $svcKey = "HKLM:\SYSTEM\CurrentControlSet\Services\" + $name
            $startValue = Get-RegistryDword $svcKey "Start"
            $imagePath = Get-RegistryString $svcKey "ImagePath"
            $group = Get-RegistryString $svcKey "Group"
            $type = Get-RegistryDword $svcKey "Type"
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
        if ($found.registry_start_type) {
            $svcDetails += ("Registry Start=" + $found.registry_start_value + " (" + $found.registry_start_type + ")")
        }
        if ($found.registry_image_path) {
            $svcDetails += ("Registry ImagePath=" + $found.registry_image_path)
        }
        if ($resolvedImagePath) {
            $svcDetails += ("Resolved ImagePath=" + $resolvedImagePath + " (exists=" + $resolvedImageExists + ")")
        }
        $svcDetails += ("Expected driver file=" + $expectedSysPath + " (exists=" + $expectedSysExists + ")")
        if (-not $expectedSysExists -and ($resolvedImageExists -ne $true)) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $svcDetails += "Storage driver binary not found under System32\\drivers. Switching the boot disk to virtio-blk may fail (0x7B). Re-run setup.cmd."
        }
        if ($found.registry_start_value -ne $null -and $found.registry_start_value -ne 0) {
            $svcStatus = Merge-Status $svcStatus "WARN"
            $svcDetails += "Storage service is not configured as BOOT_START (Start=0). Switching the boot disk to virtio-blk may fail (0x7B). Re-run setup.cmd."
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

        $records = @()
        $missing = 0
        $mismatch = 0

        foreach ($hwid in $cfgVirtioBlkHwids) {
            $baseKey = $hwid.Replace("\", "#")
            foreach ($suffix in @("", "&CC_010000", "&CC_0100")) {
                $keyName = $baseKey + $suffix
                $path = Join-Path $basePath $keyName

                $exists = Test-Path $path
                $svc = $null
                if ($exists) {
                    try {
                        $svc = (Get-ItemProperty -Path $path -ErrorAction Stop).Service
                    } catch {
                        $svc = $null
                    }
                    if ($svc -and ($svc.ToLower() -ne $expectedService.ToLower())) { $mismatch++ }
                } else {
                    $missing++
                }

                $records += @{
                    key = $keyName
                    exists = $exists
                    service = $svc
                    expected_service = $expectedService
                }
            }
        }

        $status = "PASS"
        if ($missing -gt 0 -or $mismatch -gt 0) { $status = "WARN" }

        $summary = "Checked " + $records.Count + " CriticalDeviceDatabase key(s) for service '" + $expectedService + "' (missing: " + $missing + ", mismatched service: " + $mismatch + ")"
        $details = @()
        if ($missing -gt 0) { $details += "Missing CriticalDeviceDatabase keys can cause 0x7B (INACCESSIBLE_BOOT_DEVICE) when switching the boot disk to virtio-blk." }
        if ($mismatch -gt 0) { $details += "Some keys do not map to the expected storage service; re-run setup.cmd and verify config\\devices.cmd matches your storage driver's INF AddService name." }

        $data = @{
            config_file = $gtConfig.file_path
            config_service = $expectedService
            configured_hwids = $cfgVirtioBlkHwids
            checked_keys = $records
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
