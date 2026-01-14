@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem Aero Guest Tools installer for Windows 7 SP1 (x86/x64).
rem Offline + built-in tooling only: certutil, pnputil, reg, bcdedit, shutdown.

rem Standard exit codes (stable for automation/scripted use).
set "EC_ADMIN_REQUIRED=10"
set "EC_DRIVER_DIR_MISSING=11"
set "EC_CERTS_MISSING=12"
set "EC_STORAGE_SERVICE_MISMATCH=13"
set "EC_MEDIA_INTEGRITY_FAILED=14"

set "SCRIPT_DIR=%~dp0"

rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"

rem Internal CI/self-test hook: run the INF AddService parser without admin/side effects.
rem Not a stable interface; used by tools/guest-tools/tests on Windows runners.
if /i "%~1"=="/_selftest_inf_addservice" goto :_selftest_inf_addservice
if /i "%~1"=="/_selftest_validate_storage_service_infs" goto :_selftest_validate_storage_service_infs

pushd "%SCRIPT_DIR%" >nul 2>&1
if errorlevel 1 (
  echo ERROR: Could not cd to "%SCRIPT_DIR%".
  exit /b 1
)

set "INSTALL_ROOT=C:\AeroGuestTools"
set "LOG=%INSTALL_ROOT%\install.log"
set "PKG_LIST=%INSTALL_ROOT%\installed-driver-packages.txt"
set "CERT_LIST=%INSTALL_ROOT%\installed-certs.txt"
set "STATE_INSTALLED_MEDIA=%INSTALL_ROOT%\installed-media.txt"
set "STATE_TESTSIGN=%INSTALL_ROOT%\testsigning.enabled-by-aero.txt"
set "STATE_NOINTEGRITY=%INSTALL_ROOT%\nointegritychecks.enabled-by-aero.txt"
set "STATE_STORAGE_SKIPPED=%INSTALL_ROOT%\storage-preseed.skipped.txt"

set "ARG_FORCE=0"
set "ARG_STAGE_ONLY=0"
set "ARG_FORCE_TESTSIGN=0"
set "ARG_SKIP_TESTSIGN=0"
set "ARG_FORCE_NOINTEGRITY=0"
set "ARG_FORCE_SIGNING_POLICY="
set "ARG_INSTALL_CERTS=0"
set "ARG_NO_REBOOT=0"
set "ARG_SKIP_STORAGE=0"
set "ARG_CHECK=0"
set "ARG_VERIFY_MEDIA=0"

rem Default signing policy (back-compat if manifest.json is missing/old).
set "SIGNING_POLICY=test"

set "REBOOT_REQUIRED=0"
set "CHANGED_TESTSIGNING=0"
set "CHANGED_NOINTEGRITY=0"

if /i "%~1"=="/?" goto :usage
if /i "%~1"=="-h" goto :usage
if /i "%~1"=="--help" goto :usage

for %%A in (%*) do (
  if /i "%%~A"=="/force" set "ARG_FORCE=1"
  if /i "%%~A"=="/quiet" set "ARG_FORCE=1"
  if /i "%%~A"=="/stageonly" set "ARG_STAGE_ONLY=1"
  if /i "%%~A"=="/stage-only" set "ARG_STAGE_ONLY=1"
  if /i "%%~A"=="/check" set "ARG_CHECK=1"
  if /i "%%~A"=="/validate" set "ARG_CHECK=1"
  if /i "%%~A"=="/testsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/forcetestsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/force-testsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/notestsigning" set "ARG_SKIP_TESTSIGN=1"
  if /i "%%~A"=="/no-testsigning" set "ARG_SKIP_TESTSIGN=1"
  if /i "%%~A"=="/nointegritychecks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/forcenointegritychecks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/no-integrity-checks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/forcesigningpolicy:none" set "ARG_FORCE_SIGNING_POLICY=none"
  if /i "%%~A"=="/forcesigningpolicy:test" set "ARG_FORCE_SIGNING_POLICY=test"
  if /i "%%~A"=="/forcesigningpolicy:production" set "ARG_FORCE_SIGNING_POLICY=production"
  rem Legacy aliases:
  if /i "%%~A"=="/forcesigningpolicy:testsigning" set "ARG_FORCE_SIGNING_POLICY=test"
  if /i "%%~A"=="/forcesigningpolicy:nointegritychecks" set "ARG_FORCE_SIGNING_POLICY=none"
  if /i "%%~A"=="/installcerts" set "ARG_INSTALL_CERTS=1"
  if /i "%%~A"=="/install-certs" set "ARG_INSTALL_CERTS=1"
  if /i "%%~A"=="/noreboot" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/no-reboot" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/skipstorage" set "ARG_SKIP_STORAGE=1"
  if /i "%%~A"=="/skip-storage" set "ARG_SKIP_STORAGE=1"
  if /i "%%~A"=="/verify-media" set "ARG_VERIFY_MEDIA=1"
  if /i "%%~A"=="/verifymedia" set "ARG_VERIFY_MEDIA=1"
)

rem /force implies fully non-interactive behavior.
if "%ARG_FORCE%"=="1" (
  set "ARG_NO_REBOOT=1"
)

rem Non-destructive validation mode for automation/cautious users.
rem Must run before any Administrator checks or system modifications.
if "%ARG_CHECK%"=="1" goto :check_mode

call :require_admin_stdout
if errorlevel 1 (
  set "RC=%ERRORLEVEL%"
  popd >nul 2>&1
  exit /b %RC%
)
if "%ARG_VERIFY_MEDIA%"=="1" (
  call :verify_media_preflight
  if errorlevel 1 (
    set "RC=%ERRORLEVEL%"
    popd >nul 2>&1
    exit /b %RC%
  )
)
call :init_logging
if errorlevel 1 (
  popd >nul 2>&1
  exit /b 1
)
call :log "Aero Guest Tools setup starting..."
call :log "Script dir: %SCRIPT_DIR%"
call :log "System tools: %SYS32%"
call :log "Logs: %LOG%"
call :log_manifest
call :warn_if_installed_media_mismatch
if "%ARG_SKIP_STORAGE%"=="0" (
  rem Clear any stale marker from a previous /skipstorage run as early as possible so
  rem diagnostics reflect the current invocation even if setup fails before the storage step.
  call :clear_storage_skip_marker
)

call :require_admin || goto :fail
call :detect_arch || goto :fail
call :load_signing_policy
call :warn_if_unexpected_certs
call :apply_force_defaults || goto :fail
call :load_config || goto :fail
if "%ARG_SKIP_STORAGE%"=="1" (
  call :log ""
  call :log "Skipping virtio-blk storage INF validation (/skipstorage)."
  call :log "WARNING: Boot-critical virtio-blk pre-seeding is disabled; do NOT switch the boot disk to virtio-blk."
) else (
  call :validate_storage_service_infs || goto :fail
)
call :check_kb3033929

call :install_certs || goto :fail
call :maybe_enable_testsigning || goto :fail
call :stage_all_drivers || goto :fail
if "%ARG_SKIP_STORAGE%"=="1" (
  call :skip_storage_preseed || goto :fail
) else (
  call :clear_storage_skip_marker
  call :preseed_storage_boot || goto :fail
)

call :write_installed_media_state

call :log ""
call :log "Setup complete."
call :log "Next steps:"
call :log "  1) Power off or reboot the VM."
if "%ARG_SKIP_STORAGE%"=="1" (
  call :log "  2) Switch NON-BOOT devices to virtio (net/snd/input) and Aero GPU."
  call :log "     Leave the boot disk on AHCI. Storage pre-seeding was skipped (/skipstorage)."
) else (
  call :log "  2) Switch devices to virtio (blk/net/snd/input) and Aero GPU."
)
call :log "  3) Boot Windows; Plug and Play should bind the devices to Aero drivers."
call :log ""
if "%ARG_SKIP_STORAGE%"=="1" (
  call :log "WARNING: Boot-critical virtio-blk storage pre-seeding was skipped."
  call :log "         Switching the boot disk from AHCI -> virtio-blk may BSOD with 0x7B (INACCESSIBLE_BOOT_DEVICE)"
  call :log "         because the required registry/service plumbing was not written."
  call :log "         If you later want to boot from virtio-blk, re-run setup.cmd without /skipstorage using a Guest Tools build"
  call :log "         that includes the virtio-blk storage driver."
  call :log ""
)
call :log "Recovery if boot fails after switching storage to virtio-blk:"
call :log "  - switch storage back to AHCI and boot"
call :log "  - review %LOG%"
call :log "  - if using test-signed/custom-signed drivers on Win7 x64: enable Test Signing or nointegritychecks"

call :log_summary
call :maybe_reboot
popd >nul 2>&1
exit /b 0

:usage
echo Usage: setup.cmd [options]
echo.
echo Options:
echo   /force, /quiet        Non-interactive: implies /noreboot.
echo                        For signing_policy=test, also enables /testsigning on x64
echo                        (unless /notestsigning is provided).
echo   /check, /validate     Validate Guest Tools media payloads only (no system changes; does not require admin)
echo   /stageonly           Only stage drivers into the Driver Store (no install attempts)
echo   /testsigning         Enable test signing on x64 without prompting (overrides manifest)
echo   /forcetestsigning    Same as /testsigning
echo   /notestsigning       Skip enabling test signing (x64)
echo   /nointegritychecks   Disable signature enforcement (x64; not recommended; overrides manifest)
echo   /forcenointegritychecks  Same as /nointegritychecks
echo   /forcesigningpolicy:none^|test^|production
echo                        Override the signing_policy read from manifest.json (if present)
echo                        (legacy aliases: testsigning=test, nointegritychecks=none)
echo   /installcerts        Force installing certificates from certs\ even when signing_policy is production^|none (advanced; not recommended)
echo   /verify-media        Verify Guest Tools media integrity (manifest.json SHA-256 file hashes) before installing
echo   /noreboot            Do not prompt to reboot/shutdown at the end
echo   /skipstorage         Skip boot-critical virtio-blk storage pre-seeding (alias: /skip-storage; advanced; unsafe to switch boot disk to virtio-blk)
echo.
echo Logs are written to C:\AeroGuestTools\install.log (install mode)
echo In /check mode, logs are written under %%TEMP%%\AeroGuestToolsCheck\install.log
popd >nul 2>&1
exit /b 0

:check_mode
rem /check mode validates the *media* and does not touch the local system:
rem  - no certutil -addstore
rem  - no pnputil -a/-i
rem  - no reg add/delete
rem  - no bcdedit
rem The default C:\AeroGuestTools location is typically not writable without admin, and
rem the Guest Tools media itself may be read-only (ISO). Log to %%TEMP%% instead.
set "INSTALL_ROOT=%TEMP%\AeroGuestToolsCheck"
set "LOG=%INSTALL_ROOT%\install.log"
set "PKG_LIST=%INSTALL_ROOT%\installed-driver-packages.txt"
set "CERT_LIST=%INSTALL_ROOT%\installed-certs.txt"
set "STATE_TESTSIGN=%INSTALL_ROOT%\testsigning.enabled-by-aero.txt"
set "STATE_NOINTEGRITY=%INSTALL_ROOT%\nointegritychecks.enabled-by-aero.txt"
set "STATE_STORAGE_SKIPPED=%INSTALL_ROOT%\storage-preseed.skipped.txt"

call :init_logging
if errorlevel 1 (
  popd >nul 2>&1
  exit /b 1
)

call :log "Aero Guest Tools validation starting (/check)..."
call :log "Script dir: %SCRIPT_DIR%"
call :log "System tools: %SYS32%"
call :log "Logs: %LOG%"

rem Best-effort admin probe: /check does not require elevation.
"%SYS32%\fsutil.exe" dirty query %SYSTEMDRIVE% >nul 2>&1
if errorlevel 1 (
  call :log "INFO: Not running as Administrator. Some checks are intentionally skipped in /check mode."
) else (
  call :log "INFO: Administrator privileges detected (not required for /check)."
)

if "%ARG_VERIFY_MEDIA%"=="1" (
  call :verify_media_preflight
  if errorlevel 1 goto :check_fail
)

call :log_manifest
if not defined GT_MANIFEST (
  call :log "INFO: Guest Tools manifest.json not found; using legacy defaults."
)
call :warn_if_installed_media_mismatch
call :validate_manifest_signing_policy || goto :check_fail
call :detect_arch || goto :check_fail
call :load_signing_policy
call :load_config || goto :check_fail
if "%ARG_SKIP_STORAGE%"=="1" (
  call :log ""
  call :log "Skipping virtio-blk storage INF validation (/skipstorage)."
) else (
  call :validate_storage_service_infs || goto :check_fail
)
call :validate_cert_payload || goto :check_fail

call :log ""
call :log "Validation complete. No system changes were made."
popd >nul 2>&1
exit /b 0

:check_fail
set "RC=%ERRORLEVEL%"
call :log ""
call :log "ERROR: validation failed (exit code %RC%). See %LOG% for details."
popd >nul 2>&1
exit /b %RC%

:_selftest_inf_addservice
rem Usage:
rem   setup.cmd /_selftest_inf_addservice <inf> <service>
rem   setup.cmd /_selftest_inf_addservice /bomless_utf16le <service>
rem   setup.cmd /_selftest_inf_addservice /bomless_utf16be <service>
rem
rem Tip: to exercise UTF-16-without-BOM handling with an arbitrary fixture INF, write the
rem INF as UTF-16LE or UTF-16BE without a BOM (e.g. in Python:
rem   Path('x.inf').write_bytes('...'.encode('utf-16-le'))
if "%~2"=="" (
  echo ERROR: Missing INF path.
  exit /b 2
)
if "%~3"=="" (
  echo ERROR: Missing service name.
  exit /b 2
)
if /i "%~2"=="/bomless_utf16le" (
  call :_selftest_inf_addservice_generated "le" "%~3"
  exit /b %ERRORLEVEL%
)
if /i "%~2"=="/bomless_utf16be" (
  call :_selftest_inf_addservice_generated "be" "%~3"
  exit /b %ERRORLEVEL%
)
call :inf_contains_addservice "%~2" "%~3"
exit /b %ERRORLEVEL%

:_selftest_inf_addservice_generated
setlocal EnableDelayedExpansion
set "ENC=%~1"
set "SVC=%~2"
set "TMP_INF=%TEMP%\aerogt_bomless_inf_%RANDOM%.inf"

set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PWSH%" set "PWSH=powershell.exe"

set "AEROGT_SELFTEST_INF=%TMP_INF%"
set "AEROGT_SELFTEST_SVC=%SVC%"
set "AEROGT_SELFTEST_ENC=%ENC%"

rem Generate a minimal INF fixture encoded as UTF-16LE/BE without a BOM.
"%PWSH%" -NoProfile -ExecutionPolicy Bypass -Command "$path=$env:AEROGT_SELFTEST_INF; $svc=$env:AEROGT_SELFTEST_SVC; $enc=$env:AEROGT_SELFTEST_ENC; $crlf=[System.Environment]::NewLine; $q=[char]34; $lines=@('; UTF-16 INF fixture (no BOM)','[DefaultInstall.NT]',('AddService = ' + $q + $svc + $q + ', 0x00000002, Service_Inst')); $text=[string]::Join($crlf,$lines)+$crlf; $e=if($enc -eq 'be'){[System.Text.Encoding]::BigEndianUnicode}else{[System.Text.Encoding]::Unicode}; [System.IO.File]::WriteAllBytes($path,$e.GetBytes($text)); exit 0" >nul 2>&1
if errorlevel 1 (
  del /q "%TMP_INF%" >nul 2>&1
  endlocal & exit /b 1
)

call :inf_contains_addservice "%TMP_INF%" "%SVC%"
set "RC=%ERRORLEVEL%"
del /q "%TMP_INF%" >nul 2>&1
endlocal & exit /b %RC%

:_selftest_validate_storage_service_infs
rem Usage: setup.cmd /_selftest_validate_storage_service_infs <driver_dir> <service>
rem Runs only the validation logic without requiring admin or touching the system.
if "%~2"=="" (
  echo ERROR: Missing driver_dir.
  exit /b 2
)
if "%~3"=="" (
  echo ERROR: Missing service name.
  exit /b 2
)
set "DRIVER_DIR=%~2"
set "AERO_VIRTIO_BLK_SERVICE=%~3"
set "INSTALL_ROOT=%TEMP%\aerogt_selftest_%RANDOM%"
set "LOG=%INSTALL_ROOT%\install.log"
if not exist "%INSTALL_ROOT%" mkdir "%INSTALL_ROOT%" >nul 2>&1
call :init_logging
call :validate_storage_service_infs
exit /b %ERRORLEVEL%

:fail
set "RC=%ERRORLEVEL%"
call :log ""
call :log "ERROR: setup failed (exit code %RC%). See %LOG% for details."
call :log "Recovery: do NOT switch storage to virtio-blk until setup completes successfully."
popd >nul 2>&1
exit /b %RC%

:init_logging
if not exist "%INSTALL_ROOT%" mkdir "%INSTALL_ROOT%" >nul 2>&1
if not exist "%INSTALL_ROOT%" (
  echo ERROR: Could not create "%INSTALL_ROOT%".
  exit /b 1
)
>>"%LOG%" echo ============================================================
>>"%LOG%" echo [%DATE% %TIME%] Aero Guest Tools setup starting
>>"%LOG%" echo ============================================================
exit /b 0

:log
setlocal DisableDelayedExpansion
echo(%*
>>"%LOG%" echo(%*
endlocal & exit /b 0

:require_admin_stdout
"%SYS32%\fsutil.exe" dirty query %SYSTEMDRIVE% >nul 2>&1
if errorlevel 1 (
  echo ERROR: Administrator privileges are required.
  echo Right-click setup.cmd and choose 'Run as administrator'.
  exit /b %EC_ADMIN_REQUIRED%
)
exit /b 0

:verify_media_preflight
setlocal EnableDelayedExpansion
echo.
echo Verifying Guest Tools media integrity ^(/verify-media^)...

rem Locate manifest.json (either alongside setup.cmd or one directory above).
set "MEDIA_ROOT="
for %%I in ("%SCRIPT_DIR%..") do set "MEDIA_ROOT=%%~fI"
set "MANIFEST=%SCRIPT_DIR%manifest.json"
set "ROOT=%SCRIPT_DIR%"
if not exist "!MANIFEST!" (
  set "MANIFEST=!MEDIA_ROOT!\manifest.json"
  set "ROOT=!MEDIA_ROOT!"
)

if not exist "!MANIFEST!" (
  echo ERROR: manifest.json not found; cannot verify Guest Tools media integrity.
  echo Expected:
  echo   "!MEDIA_ROOT!\manifest.json"
  echo   "%SCRIPT_DIR%manifest.json"
  echo Remediation: replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.
  endlocal & exit /b %EC_MEDIA_INTEGRITY_FAILED%
)

echo Manifest: "!MANIFEST!"

call :verify_media_with_powershell "!MANIFEST!" "!ROOT!"
set "RC=!ERRORLEVEL!"
if "!RC!"=="0" (
  endlocal & exit /b 0
)
if "!RC!"=="%EC_MEDIA_INTEGRITY_FAILED%" (
  endlocal & exit /b %EC_MEDIA_INTEGRITY_FAILED%
)

rem In /check mode, avoid certutil-based hashing fallbacks to keep validation non-destructive.
rem PowerShell is expected to be available on Windows 7 (inbox) for /verify-media.
if "%ARG_CHECK%"=="1" (
  echo ERROR: PowerShell media verification unavailable or failed (exit code !RC!); cannot verify media integrity in /check mode.
  echo Remediation: run /verify-media without /check, or run /check on a system with PowerShell 2.0 available.
  endlocal & exit /b %EC_MEDIA_INTEGRITY_FAILED%
)

echo WARNING: PowerShell media verification unavailable or failed (exit code !RC!); falling back to CMD parser.
call :verify_media_with_cmd "!MANIFEST!" "!ROOT!"
set "RC=!ERRORLEVEL!"
endlocal & exit /b !RC!

:verify_media_with_powershell
setlocal EnableDelayedExpansion
set "MANIFEST=%~1"
set "ROOT=%~2"
set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PWSH%" set "PWSH=powershell.exe"
if not exist "%PWSH%" (
  endlocal & exit /b 2
)

set "AEROGT_MANIFEST=%MANIFEST%"
set "AEROGT_MEDIA_ROOT=%ROOT%"

rem Use PowerShell 2.0-compatible JSON parsing (JavaScriptSerializer) and SHA-256 hashing.
rem Exit codes:
rem   0  - OK
rem   14 - integrity failure (missing/mismatch)
rem   2  - PowerShell unavailable/parse error (caller may fall back to CMD parsing)
"%PWSH%" -NoProfile -ExecutionPolicy Bypass -Command "$manifest=$env:AEROGT_MANIFEST; $root=$env:AEROGT_MEDIA_ROOT; function Get-FileSha256Hex([string]$path){ try{ $stream=[System.IO.File]::OpenRead($path); try{ $sha=New-Object System.Security.Cryptography.SHA256Managed; try{ $hash=$sha.ComputeHash($stream) } finally { try{ $sha.Dispose() } catch {} } } finally { try{ $stream.Dispose() } catch {} }; $sb=New-Object System.Text.StringBuilder; foreach($b in $hash){ [void]$sb.AppendFormat('{0:x2}',$b) }; return $sb.ToString() } catch { return $null } }; function Parse-JsonCompat([string]$json){ try{ [void][System.Reflection.Assembly]::LoadWithPartialName('System.Web.Extensions'); $ser=New-Object System.Web.Script.Serialization.JavaScriptSerializer; $ser.MaxJsonLength=104857600; return $ser.DeserializeObject($json) } catch { return $null } }; try{ $json=[System.IO.File]::ReadAllText($manifest) } catch { Write-Host ('ERROR: Failed to read manifest.json: ' + $manifest); exit 2 }; $obj=Parse-JsonCompat $json; if(-not $obj){ Write-Host ('ERROR: Failed to parse manifest.json as JSON: ' + $manifest); exit 2 }; $files=$obj['files']; if(-not $files -or $files.Count -eq 0){ Write-Host 'ERROR: manifest.json contains no file entries (files[]); cannot verify media integrity.'; Write-Host 'Remediation: replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.'; exit 14 }; $total=0; $missing=0; $mismatch=0; foreach($f in $files){ $total++; $rel=(''+$f['path']).Trim(); $exp=(''+$f['sha256']).Trim(); if(-not $rel -or -not $exp){ Write-Host 'ERROR: manifest.json contains an invalid file entry (missing path/sha256).'; $mismatch++; continue }; if($rel -match '^[\\/]' -or $rel -match '^[A-Za-z]:'){ Write-Host ('ERROR: manifest.json contains an unsafe absolute path: ' + $rel); $mismatch++; continue }; if($rel -match '(\.\.[\\/])|(\.\.$)'){ Write-Host ('ERROR: manifest.json contains an unsafe path traversal: ' + $rel); $mismatch++; continue }; $relFs=$rel -replace '/', '\\'; $full=[System.IO.Path]::Combine($root, $relFs); if(-not (Test-Path -LiteralPath $full -PathType Leaf)){ Write-Host ('ERROR: Missing file: ' + $rel); $missing++; continue }; $expSize=$null; try{ $expSize=$f['size'] }catch{ $expSize=$null }; if($expSize -ne $null -and (''+$expSize).Trim().Length -gt 0){ $actSize=$null; try{ $actSize=(Get-Item -LiteralPath $full).Length }catch{ $actSize=$null }; $expSize64=$null; try{ $expSize64=[Int64]$expSize }catch{ $expSize64=$null }; if($actSize -ne $null -and $expSize64 -ne $null -and [Int64]$actSize -ne $expSize64){ Write-Host ('ERROR: Size mismatch: ' + $rel); Write-Host ('       expected: ' + $expSize64 + ' bytes'); Write-Host ('       actual:   ' + $actSize + ' bytes'); $mismatch++; continue } }; $act=Get-FileSha256Hex $full; if(-not $act){ Write-Host ('ERROR: Failed to hash file: ' + $rel); $mismatch++; continue }; if($act.ToLower() -ne $exp.ToLower()){ Write-Host ('ERROR: Hash mismatch: ' + $rel); Write-Host ('       expected: ' + $exp); Write-Host ('       actual:   ' + $act); $mismatch++; } }; Write-Host ('Media integrity summary: files checked=' + $total + ', missing=' + $missing + ', mismatched=' + $mismatch); if($missing -gt 0 -or $mismatch -gt 0){ Write-Host 'Remediation: replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.'; exit 14 }; exit 0"

endlocal & exit /b %ERRORLEVEL%

:verify_media_with_cmd
setlocal EnableDelayedExpansion
set "MANIFEST=%~1"
set "ROOT=%~2"
if not "!ROOT:~-1!"=="\" set "ROOT=!ROOT!\"

set /a TOTAL=0
set /a MISSING=0
set /a MISMATCH=0
set "IN_FILES=0"
set "SEEN_FILES_KEY=0"
set "CUR_PATH="
set "CUR_SHA="
set "CUR_SIZE="

for /f "usebackq delims=" %%L in ("!MANIFEST!") do (
  set "LINE=%%L"

  rem Only validate entries under the manifest's files[] array. Modern manifests also include
  rem other objects containing path/sha256 keys (e.g. packager inputs/provenance) which should
  rem not be treated as media file entries.
  if "!IN_FILES!"=="0" (
    echo(!LINE!| "%SYS32%\findstr.exe" /i "\"files\"" >nul 2>&1 && set "SEEN_FILES_KEY=1"
    if "!SEEN_FILES_KEY!"=="1" (
      echo(!LINE!| "%SYS32%\findstr.exe" /c:"[" >nul 2>&1 && (
        set "IN_FILES=1"
        set "SEEN_FILES_KEY=0"
        set "CUR_PATH="
        set "CUR_SHA="
        set "CUR_SIZE="
      )
    )
  ) else (
    rem End of files[] array.
    echo(!LINE!| "%SYS32%\findstr.exe" /r /c:"^[ ]*][ ]*,*[ ]*$" >nul 2>&1 && (
      set "IN_FILES=0"
      set "CUR_PATH="
      set "CUR_SHA="
      set "CUR_SIZE="
    )
  )

  if not "!IN_FILES!"=="1" (
    rem Not currently within files[]; ignore this line.
  ) else (
  echo(!LINE!| "%SYS32%\findstr.exe" /i "\"path\"" >nul 2>&1
  if not errorlevel 1 (
    set "VAL="
    for /f "tokens=1,* delims=:" %%A in ("!LINE!") do set "VAL=%%B"
    for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
    if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
    set "VAL=!VAL:"=!"
    set "CUR_PATH=!VAL!"
    set "CUR_SHA="
    set "CUR_SIZE="
  )

  echo(!LINE!| "%SYS32%\findstr.exe" /i "\"sha256\"" >nul 2>&1
  if not errorlevel 1 (
    if defined CUR_PATH (
      set "VAL="
      for /f "tokens=1,* delims=:" %%A in ("!LINE!") do set "VAL=%%B"
      for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
      if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
      set "VAL=!VAL:"=!"
      set "CUR_SHA=!VAL!"
    )
  )

  echo(!LINE!| "%SYS32%\findstr.exe" /i "\"size\"" >nul 2>&1
  if not errorlevel 1 (
    if defined CUR_PATH (
      set "VAL="
      for /f "tokens=1,* delims=:" %%A in ("!LINE!") do set "VAL=%%B"
      for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
      if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
      set "VAL=!VAL:"=!"
      set "CUR_SIZE=!VAL!"
    )
  )

  rem Verify once we've reached the end of a file entry object (normally a line like "}," or "}").
  echo(!LINE!| "%SYS32%\findstr.exe" /r /c:"^[ ]*}[ ]*,*[ ]*$" >nul 2>&1
  if not errorlevel 1 (
    if defined CUR_PATH if defined CUR_SHA (
      set /a TOTAL+=1
      call :verify_media_one_file_cmd "!CUR_PATH!" "!CUR_SHA!" "!ROOT!" "!CUR_SIZE!"
      set "RC=!ERRORLEVEL!"
      if "!RC!"=="1" set /a MISSING+=1
      if "!RC!"=="2" set /a MISMATCH+=1

      set "CUR_PATH="
      set "CUR_SHA="
      set "CUR_SIZE="
    )
  )
  )
)

if "!TOTAL!"=="0" (
  echo ERROR: No file entries found in manifest.json; cannot verify media integrity.
  echo Remediation: replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.
  endlocal & exit /b %EC_MEDIA_INTEGRITY_FAILED%
)

echo Media integrity summary: files checked=!TOTAL!, missing=!MISSING!, mismatched=!MISMATCH!
if not "!MISSING!"=="0" goto :verify_media_cmd_fail
if not "!MISMATCH!"=="0" goto :verify_media_cmd_fail
endlocal & exit /b 0

:verify_media_cmd_fail
echo Remediation: replace the Guest Tools ISO/zip with a fresh copy; do not mix driver folders across versions.
endlocal & exit /b %EC_MEDIA_INTEGRITY_FAILED%

:verify_media_one_file_cmd
setlocal EnableDelayedExpansion
set "REL=%~1"
set "EXP=%~2"
set "ROOT=%~3"
set "EXP_SIZE=%~4"

rem Basic path safety: refuse absolute paths or traversal entries.
if "!REL:~1,1!"==":" (
  echo ERROR: manifest.json contains an unsafe absolute path: !REL!
  endlocal & exit /b 2
)
if "!REL:~0,1!"=="\" (
  echo ERROR: manifest.json contains an unsafe absolute path: !REL!
  endlocal & exit /b 2
)
if "!REL:~0,1!"=="/" (
  echo ERROR: manifest.json contains an unsafe absolute path: !REL!
  endlocal & exit /b 2
)
if not "!REL:..=!"=="!REL!" (
  echo ERROR: manifest.json contains an unsafe path traversal: !REL!
  endlocal & exit /b 2
)

set "RELFS=!REL:/=\!"
set "FULL=!ROOT!!RELFS!"

if not exist "!FULL!" (
  echo ERROR: Missing file: !REL!
  endlocal & exit /b 1
)

rem Optional: validate file size if manifest.json provides it.
if defined EXP_SIZE (
  echo(!EXP_SIZE!| "%SYS32%\findstr.exe" /r /c:"^[0-9][0-9]*$" >nul 2>&1
  if not errorlevel 1 (
    set "ACT_SIZE="
    for %%F in ("!FULL!") do set "ACT_SIZE=%%~zF"
    if defined ACT_SIZE if not "!ACT_SIZE!"=="!EXP_SIZE!" (
      echo ERROR: Size mismatch: !REL!
      echo        expected: !EXP_SIZE! bytes
      echo        actual:   !ACT_SIZE! bytes
      endlocal & exit /b 2
    )
  )
)

set "ACTUAL="
for /f "usebackq delims=" %%H in (`"%SYS32%\certutil.exe" -hashfile "!FULL!" SHA256 ^| "%SYS32%\findstr.exe" /r /i "^[ ]*[0-9a-f][0-9a-f ]*$"`) do (
  if not defined ACTUAL set "ACTUAL=%%H"
)
set "ACTUAL=!ACTUAL: =!"

if not defined ACTUAL (
  echo ERROR: Failed to hash file: !REL!
  endlocal & exit /b 2
)

if /i not "!ACTUAL!"=="!EXP!" (
  echo ERROR: Hash mismatch: !REL!
  echo        expected: !EXP!
  echo        actual:   !ACTUAL!
  endlocal & exit /b 2
)

endlocal & exit /b 0

:log_manifest
setlocal EnableDelayedExpansion

rem Optional: record which Guest Tools build produced the media (if provided).
set "MEDIA_ROOT="
for %%I in ("%SCRIPT_DIR%..") do set "MEDIA_ROOT=%%~fI"
rem Prefer manifest.json next to setup.cmd. Fall back to the media root one directory above.
rem This avoids accidentally picking up an unrelated parent-directory manifest when the media
rem is extracted under a folder that also happens to contain a manifest.json.
set "MANIFEST=%SCRIPT_DIR%manifest.json"
if not exist "!MANIFEST!" set "MANIFEST=!MEDIA_ROOT!\manifest.json"
if not exist "!MANIFEST!" (
  endlocal & (
    rem Back-compat: without a manifest, assume test-signed behavior.
    set "GT_MANIFEST="
    set "GT_VERSION="
    set "GT_BUILD_ID="
    set "GT_SIGNING_POLICY=test"
    set "GT_CERTS_REQUIRED=1"
    set "GT_PARSED_SIGNING_POLICY=0"
    set "GT_PARSED_CERTS_REQUIRED=0"
  ) & exit /b 0
)

set "GT_MANIFEST=!MANIFEST!"
set "GT_VERSION="
set "GT_BUILD_ID="
set "GT_SIGNING_POLICY="
set "GT_CERTS_REQUIRED="
set "GT_PARSED_SIGNING_POLICY=0"
set "GT_PARSED_CERTS_REQUIRED=0"

rem Prefer PowerShell JSON parsing (robust to schema/formatting changes). Fall back to
rem the legacy line-based parser if PowerShell is unavailable or parsing fails.
set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PWSH%" set "PWSH=powershell.exe"
set "PWSH_OK=0"
set "PWSH_OUT=%TEMP%\aerogt_manifest_parse_%RANDOM%.txt"
set "AEROGT_MANIFEST=!MANIFEST!"
"%PWSH%" -NoProfile -ExecutionPolicy Bypass -Command "try{ $p=$env:AEROGT_MANIFEST; [void][System.Reflection.Assembly]::LoadWithPartialName('System.Web.Extensions'); $json=[System.IO.File]::ReadAllText($p); $ser=New-Object System.Web.Script.Serialization.JavaScriptSerializer; $ser.MaxJsonLength=10485760; $o=$ser.DeserializeObject($json); function g($d,$k){ if($d -is [System.Collections.IDictionary]){ $i=[System.Collections.IDictionary]$d; if($i.Contains($k)){ $i[$k] } else { $null } } else { $null } }; $pkg=g $o 'package'; $v=g $pkg 'version'; if($v -eq $null){ $v=g $o 'version' }; $b=g $pkg 'build_id'; if($b -eq $null){ $b=g $o 'build_id' }; $sp=g $o 'signing_policy'; $cr=g $o 'certs_required'; Write-Output 'AEROGT_POWERSHELL_OK=1'; Write-Output ('GT_VERSION=' + $v); Write-Output ('GT_BUILD_ID=' + $b); Write-Output ('GT_SIGNING_POLICY=' + $sp); Write-Output ('GT_CERTS_REQUIRED=' + $cr); exit 0 }catch{ exit 1 }" >"%PWSH_OUT%" 2>nul
if "%ERRORLEVEL%"=="0" (
  for /f "usebackq tokens=1,* delims==" %%A in ("%PWSH_OUT%") do (
    if /i "%%A"=="AEROGT_POWERSHELL_OK" if "%%B"=="1" set "PWSH_OK=1"
    if /i "%%A"=="GT_VERSION" set "GT_VERSION=%%B"
    if /i "%%A"=="GT_BUILD_ID" set "GT_BUILD_ID=%%B"
    if /i "%%A"=="GT_SIGNING_POLICY" set "GT_SIGNING_POLICY=%%B"
    if /i "%%A"=="GT_CERTS_REQUIRED" set "GT_CERTS_REQUIRED=%%B"
  )
)
del /q "%PWSH_OUT%" >nul 2>&1
if "%PWSH_OK%"=="1" (
  if defined GT_SIGNING_POLICY set "GT_PARSED_SIGNING_POLICY=1"
  if defined GT_CERTS_REQUIRED set "GT_PARSED_CERTS_REQUIRED=1"
)
if not "%PWSH_OK%"=="1" (
  call :log "WARNING: Failed to parse manifest.json via PowerShell; falling back to legacy parser."
  set "GT_VERSION="
  set "GT_BUILD_ID="
  set "GT_SIGNING_POLICY="
  set "GT_CERTS_REQUIRED="
  goto log_manifest_legacy_parse
)
goto log_manifest_normalize

:log_manifest_legacy_parse
for /f "usebackq tokens=1,* delims=:" %%A in ("!MANIFEST!") do (
   set "KEY=%%A"
   set "VAL=%%B"
   set "KEY=!KEY: =!"
  set "KEY=!KEY:"=!"
  set "KEY=!KEY:{=!"
  set "KEY=!KEY:}=!"
  set "KEY=!KEY:,=!"

  if /i "!KEY!"=="version" (
    set "VAL=%%B"
    for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
    if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
    set "VAL=!VAL:"=!"
    set "GT_VERSION=!VAL!"
  )
  if /i "!KEY!"=="build_id" (
    set "VAL=%%B"
    for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
    if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
    set "VAL=!VAL:"=!"
    set "GT_BUILD_ID=!VAL!"
  )
  if /i "!KEY!"=="signing_policy" (
    set "VAL=%%B"
    for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
    if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
    set "VAL=!VAL:"=!"
    set "GT_SIGNING_POLICY=!VAL!"
    set "GT_PARSED_SIGNING_POLICY=1"
  )
  if /i "!KEY!"=="certs_required" (
    set "VAL=%%B"
    for /f "tokens=* delims= " %%V in ("!VAL!") do set "VAL=%%V"
    if "!VAL:~-1!"=="," set "VAL=!VAL:~0,-1!"
    set "VAL=!VAL:"=!"
    set "GT_CERTS_REQUIRED=!VAL!"
    set "GT_PARSED_CERTS_REQUIRED=1"
  )
)

:log_manifest_normalize

rem Back-compat: old manifests don't include signing_policy/certs_required.
if not defined GT_SIGNING_POLICY set "GT_SIGNING_POLICY=test"
rem Normalize legacy signing_policy values to the current surface (test|production|none).
if /i "!GT_SIGNING_POLICY!"=="testsigning" set "GT_SIGNING_POLICY=test"
if /i "!GT_SIGNING_POLICY!"=="test-signing" set "GT_SIGNING_POLICY=test"
if /i "!GT_SIGNING_POLICY!"=="nointegritychecks" set "GT_SIGNING_POLICY=none"
if /i "!GT_SIGNING_POLICY!"=="no-integrity-checks" set "GT_SIGNING_POLICY=none"
if /i "!GT_SIGNING_POLICY!"=="prod" set "GT_SIGNING_POLICY=production"
if /i "!GT_SIGNING_POLICY!"=="whql" set "GT_SIGNING_POLICY=production"
if not defined GT_CERTS_REQUIRED (
  if /i "!GT_SIGNING_POLICY!"=="test" (
    set "GT_CERTS_REQUIRED=1"
  ) else (
    set "GT_CERTS_REQUIRED=0"
  )
)
if /i "!GT_CERTS_REQUIRED!"=="true" set "GT_CERTS_REQUIRED=1"
if /i "!GT_CERTS_REQUIRED!"=="false" set "GT_CERTS_REQUIRED=0"

if defined GT_VERSION (
  if defined GT_BUILD_ID (
    call :log "Guest Tools manifest: version=!GT_VERSION!, build_id=!GT_BUILD_ID!"
  ) else (
    call :log "Guest Tools manifest: version=!GT_VERSION!"
  )
) else if defined GT_BUILD_ID (
  call :log "Guest Tools manifest: build_id=!GT_BUILD_ID!"
) else (
  call :log "Guest Tools manifest found, but could not parse version/build_id: !MANIFEST!"
)

call :log "Guest Tools signing policy: !GT_SIGNING_POLICY! (certs_required=!GT_CERTS_REQUIRED!)"

endlocal & (
  set "GT_MANIFEST=%GT_MANIFEST%"
  set "GT_VERSION=%GT_VERSION%"
  set "GT_BUILD_ID=%GT_BUILD_ID%"
  set "GT_SIGNING_POLICY=%GT_SIGNING_POLICY%"
  set "GT_CERTS_REQUIRED=%GT_CERTS_REQUIRED%"
  set "GT_PARSED_SIGNING_POLICY=%GT_PARSED_SIGNING_POLICY%"
  set "GT_PARSED_CERTS_REQUIRED=%GT_PARSED_CERTS_REQUIRED%"
) & exit /b 0

:warn_if_installed_media_mismatch
rem Best-effort "mixed media" warning: if setup.cmd was previously run from one Guest Tools build,
rem warn when the currently-running media does not match the recorded installed-media.txt.
rem This helps catch cases where users merge folders from different ISO/zip versions.
setlocal EnableDelayedExpansion
if not exist "%STATE_INSTALLED_MEDIA%" (
  endlocal & exit /b 0
)

set "IM_VERSION="
set "IM_BUILD_ID="
set "IM_SIGNING_POLICY="
set "IM_MANIFEST_SHA256="
set "CUR_MANIFEST_SHA256="
for /f "tokens=1,* delims==" %%A in ('type "%STATE_INSTALLED_MEDIA%" 2^>nul') do (
  if /i "%%A"=="GT_VERSION" set "IM_VERSION=%%B"
  if /i "%%A"=="GT_BUILD_ID" set "IM_BUILD_ID=%%B"
  if /i "%%A"=="GT_SIGNING_POLICY" set "IM_SIGNING_POLICY=%%B"
  if /i "%%A"=="manifest_sha256" set "IM_MANIFEST_SHA256=%%B"
)

set "MISMATCH=0"
set "REASON="

if defined IM_VERSION if defined GT_VERSION (
  if /i not "!IM_VERSION!"=="!GT_VERSION!" (
    set "MISMATCH=1"
    set "REASON=version mismatch"
  )
)
if defined IM_BUILD_ID if defined GT_BUILD_ID (
  if /i not "!IM_BUILD_ID!"=="!GT_BUILD_ID!" (
    set "MISMATCH=1"
    if defined REASON (set "REASON=!REASON!, build_id mismatch") else set "REASON=build_id mismatch"
  )
)
if defined IM_SIGNING_POLICY if defined GT_SIGNING_POLICY (
  if /i not "!IM_SIGNING_POLICY!"=="!GT_SIGNING_POLICY!" (
    set "MISMATCH=1"
    if defined REASON (set "REASON=!REASON!, signing_policy mismatch") else set "REASON=signing_policy mismatch"
  )
)

rem Optional: compare manifest SHA-256 when available (useful when version/build_id are missing or reused).
if defined IM_MANIFEST_SHA256 if defined GT_MANIFEST if exist "!GT_MANIFEST!" (
  if "%ARG_CHECK%"=="1" (
    rem /check must not use certutil; use PowerShell hashing when available.
    set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
    if not exist "!PWSH!" set "PWSH=powershell.exe"
    set "AEROGT_HASH_FILE=!GT_MANIFEST!"
    for /f "usebackq delims=" %%H in (`"!PWSH!" -NoProfile -ExecutionPolicy Bypass -Command "$p=$env:AEROGT_HASH_FILE; try{ $stream=[System.IO.File]::OpenRead($p); try{ $sha=New-Object System.Security.Cryptography.SHA256Managed; try{ $hash=$sha.ComputeHash($stream) } finally { try{ $sha.Dispose() } catch {} } } finally { try{ $stream.Dispose() } catch {} }; $sb=New-Object System.Text.StringBuilder; foreach($b in $hash){ [void]$sb.AppendFormat('{0:x2}',$b) }; $sb.ToString() } catch { }" 2^>nul`) do (
      if not defined CUR_MANIFEST_SHA256 set "CUR_MANIFEST_SHA256=%%H"
    )
    if defined CUR_MANIFEST_SHA256 (
      echo(!CUR_MANIFEST_SHA256!| "%SYS32%\findstr.exe" /i /r /c:"^[0-9a-f][0-9a-f]*$" >nul 2>&1
      if errorlevel 1 set "CUR_MANIFEST_SHA256="
      rem SHA-256 should be exactly 64 hex chars; reject anything else (including error output).
      if defined CUR_MANIFEST_SHA256 if "!CUR_MANIFEST_SHA256:~63,1!"=="" set "CUR_MANIFEST_SHA256="
      if defined CUR_MANIFEST_SHA256 if not "!CUR_MANIFEST_SHA256:~64,1!"=="" set "CUR_MANIFEST_SHA256="
    )
  ) else (
    if exist "%SYS32%\certutil.exe" (
      for /f "usebackq delims=" %%H in (`"%SYS32%\certutil.exe" -hashfile "!GT_MANIFEST!" SHA256 ^| "%SYS32%\findstr.exe" /r /i "^[ ]*[0-9a-f][0-9a-f ]*$"`) do (
        if not defined CUR_MANIFEST_SHA256 set "CUR_MANIFEST_SHA256=%%H"
      )
      set "CUR_MANIFEST_SHA256=!CUR_MANIFEST_SHA256: =!"
    )
  )
  if defined CUR_MANIFEST_SHA256 (
    if /i not "!CUR_MANIFEST_SHA256!"=="!IM_MANIFEST_SHA256!" (
      set "MISMATCH=1"
      if defined REASON (set "REASON=!REASON!, manifest_sha256 mismatch") else set "REASON=manifest_sha256 mismatch"
    )
  )
)

if "%MISMATCH%"=="1" (
  call :log ""
  call :log "WARNING: Installed media differs from the current Guest Tools media (!REASON!)."
  call :log "         This can indicate mixed/corrupted media (merged ISO/zip versions)."
  call :log "         Installed media record: %STATE_INSTALLED_MEDIA%"
  if defined IM_VERSION call :log "         installed-media GT_VERSION=!IM_VERSION!"
  if defined IM_BUILD_ID call :log "         installed-media GT_BUILD_ID=!IM_BUILD_ID!"
  if defined IM_SIGNING_POLICY call :log "         installed-media GT_SIGNING_POLICY=!IM_SIGNING_POLICY!"
  if defined IM_MANIFEST_SHA256 call :log "         installed-media manifest_sha256=!IM_MANIFEST_SHA256!"
  if defined GT_VERSION call :log "         current media  GT_VERSION=!GT_VERSION!"
  if defined GT_BUILD_ID call :log "         current media  GT_BUILD_ID=!GT_BUILD_ID!"
  if defined GT_SIGNING_POLICY call :log "         current media  GT_SIGNING_POLICY=!GT_SIGNING_POLICY!"
  if defined CUR_MANIFEST_SHA256 call :log "         current media  manifest_sha256=!CUR_MANIFEST_SHA256!"
)

endlocal & exit /b 0

:write_installed_media_state
setlocal EnableDelayedExpansion
set "OUT=%STATE_INSTALLED_MEDIA%"
set "MANIFEST_SHA256="

rem Optional: record the manifest SHA-256 to help diagnose mixed/corrupted media even when
rem version/build_id are ambiguous.
if defined GT_MANIFEST (
  if exist "!GT_MANIFEST!" if exist "%SYS32%\certutil.exe" (
    for /f "usebackq delims=" %%H in (`"%SYS32%\certutil.exe" -hashfile "!GT_MANIFEST!" SHA256 ^| "%SYS32%\findstr.exe" /r /i "^[ ]*[0-9a-f][0-9a-f ]*$"`) do (
      if not defined MANIFEST_SHA256 set "MANIFEST_SHA256=%%H"
    )
    set "MANIFEST_SHA256=!MANIFEST_SHA256: =!"
  )
)

rem Record which Guest Tools media build ran setup.cmd (helps diagnose mixed ISO/zip problems).
rem This is intentionally plain text so it can be attached to bug reports and parsed by verify.ps1.
> "!OUT!" (
  echo timestamp_local=!DATE! !TIME!
  if defined GT_MANIFEST (
    echo manifest_path=!GT_MANIFEST!
  ) else (
    echo manifest_path=manifest not found
  )
  echo manifest_sha256=!MANIFEST_SHA256!
  echo GT_VERSION=!GT_VERSION!
  echo GT_BUILD_ID=!GT_BUILD_ID!
  echo GT_SIGNING_POLICY=!GT_SIGNING_POLICY!
  echo GT_CERTS_REQUIRED=!GT_CERTS_REQUIRED!
  echo effective_SIGNING_POLICY=%SIGNING_POLICY%
)

if exist "!OUT!" (
  call :log "Wrote installed media state: !OUT!"
  call :log "  - manifest_path=!GT_MANIFEST!"
  call :log "  - GT_VERSION=!GT_VERSION!, GT_BUILD_ID=!GT_BUILD_ID!"
) else (
  call :log "WARNING: Failed to write installed media state: !OUT!"
)

endlocal & exit /b 0

:log_summary
call :log ""
call :log "==================== Summary ===================="
call :log "OS architecture: %OS_ARCH%"
call :log "Effective signing_policy: %SIGNING_POLICY%"
call :log "Storage service: %AERO_VIRTIO_BLK_SERVICE%"
if "%ARG_SKIP_STORAGE%"=="1" (
  call :log "Storage preseed: skipped (/skipstorage)"
  call :log virtio-blk HWIDs: %AERO_VIRTIO_BLK_HWIDS%
) else (
  call :log Seeded HWIDs: %AERO_VIRTIO_BLK_HWIDS%
)
if "%CHANGED_TESTSIGNING%"=="1" (
  call :log "testsigning:      enabled by this run"
) else (
  call :log "testsigning:      unchanged"
)
if "%CHANGED_NOINTEGRITY%"=="1" (
  call :log "nointegritychecks: enabled by this run"
) else (
  call :log "nointegritychecks: unchanged"
)
call :log "================================================="
exit /b 0

:require_admin
call :log "Checking for Administrator privileges..."
"%SYS32%\fsutil.exe" dirty query %SYSTEMDRIVE% >nul 2>&1
if errorlevel 1 (
  call :log "ERROR: Administrator privileges are required."
  call :log "Right-click setup.cmd and choose 'Run as administrator'."
  exit /b %EC_ADMIN_REQUIRED%
)
exit /b 0

:detect_arch
set "OS_ARCH=x86"
if /i "%PROCESSOR_ARCHITECTURE%"=="AMD64" set "OS_ARCH=amd64"
if /i "%PROCESSOR_ARCHITEW6432%"=="AMD64" set "OS_ARCH=amd64"
call :log "Detected OS architecture: %OS_ARCH%"
set "DRIVER_DIR=%SCRIPT_DIR%drivers\%OS_ARCH%"
if not exist "%DRIVER_DIR%" (
  call :log "ERROR: Driver directory not found: %DRIVER_DIR%"
  exit /b %EC_DRIVER_DIR_MISSING%
)
exit /b 0

:apply_force_defaults
if not "%ARG_FORCE%"=="1" exit /b 0

if /i not "%OS_ARCH%"=="amd64" exit /b 0

rem /force implies /testsigning on x64 only for signing_policy=test, unless the operator
rem explicitly disables it.
if /i not "%SIGNING_POLICY%"=="test" (
  call :log "Force mode: signing_policy=%SIGNING_POLICY%; leaving Test Signing unchanged."
  exit /b 0
)
if "%ARG_SKIP_TESTSIGN%"=="1" (
  call :log "Force mode: /notestsigning specified; leaving test signing unchanged."
  exit /b 0
)
if "%ARG_FORCE_NOINTEGRITY%"=="1" (
  call :log "Force mode: /nointegritychecks specified; leaving test signing unchanged."
  exit /b 0
)
if "%ARG_FORCE_TESTSIGN%"=="1" (
  call :log "Force mode: will enable Test Signing on x64 (explicit)."
  exit /b 0
)
set "ARG_FORCE_TESTSIGN=1"
call :log "Force mode: will enable Test Signing on x64 (implied)."
exit /b 0

:load_config
set "CONFIG_FILE=%SCRIPT_DIR%config\devices.cmd"
if not exist "%CONFIG_FILE%" (
  call :log "ERROR: Missing config file: %CONFIG_FILE%"
  exit /b 1
)
call :log "Loading config: %CONFIG_FILE%"
call "%CONFIG_FILE%"

if not defined AERO_VIRTIO_BLK_SERVICE (
  call :log "ERROR: AERO_VIRTIO_BLK_SERVICE is not set in %CONFIG_FILE%"
  exit /b 1
)
if not defined AERO_VIRTIO_BLK_HWIDS (
  call :log "ERROR: AERO_VIRTIO_BLK_HWIDS is not set in %CONFIG_FILE%"
  exit /b 1
)
exit /b 0

:check_kb3033929
rem SHA-2 / SHA-256 signing prerequisites for Windows 7:
rem - KB3033929 (adds SHA-256 signature validation support)
rem - KB4474419 (SHA-2 code signing support update)
rem - KB4490628 (servicing stack prerequisite often needed to install KB4474419)
rem If driver catalogs are SHA-256 signed and these updates are missing, Device Manager may
rem report Code 52 (signature verification failure).
call :log ""
call :log "Checking for Windows 7 signing prerequisites (KB3033929/KB4474419/KB4490628)..."

if not exist "%SYS32%\wmic.exe" (
  call :log "WARNING: wmic.exe not found; cannot detect installed hotfixes (KB3033929/KB4474419/KB4490628)."
  call :log "         See: docs/windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support"
  exit /b 0
)

set "KB_MISSING=0"

rem Wrong system time can also break signature validation (cert validity windows, timestamping).
rem Do a quick sanity check for obviously-wrong clocks without requiring locale-specific parsing.
setlocal EnableDelayedExpansion
set "LOCALDT="
for /f "tokens=2 delims==" %%D in ('"%SYS32%\wmic.exe" os get LocalDateTime /value 2^>nul ^| find "="') do (
  if not defined LOCALDT set "LOCALDT=%%D"
)
if defined LOCALDT (
  set "YEAR=!LOCALDT:~0,4!"
  set /a YEAR_NUM=!YEAR! >nul 2>&1
  if not "!YEAR_NUM!"=="" (
    if !YEAR_NUM! LSS 2010 (
      endlocal & call :log "WARNING: System clock appears to be very old (year < 2010). Incorrect time can break signature validation (Code 52)."
      goto :check_kb3033929_clock_done
    )
    if !YEAR_NUM! GTR 2050 (
      endlocal & call :log "WARNING: System clock appears to be far in the future (year > 2050). Incorrect time can break signature validation (Code 52)."
      goto :check_kb3033929_clock_done
    )
  )
)
endlocal
:check_kb3033929_clock_done

"%SYS32%\wmic.exe" qfe get HotFixID 2>nul | findstr /i "KB3033929" >nul 2>&1
if errorlevel 1 (
  set "KB_MISSING=1"
  call :log "WARNING: KB3033929 not detected. Windows may be unable to verify SHA-256-signed driver catalogs (Code 52)."
) else (
  call :log "KB3033929 detected."
)

"%SYS32%\wmic.exe" qfe get HotFixID 2>nul | findstr /i "KB4474419" >nul 2>&1
if errorlevel 1 (
  set "KB_MISSING=1"
  call :log "WARNING: KB4474419 not detected. This SHA-2 support update is often required for newer SHA-2 signatures (Code 52)."
) else (
  call :log "KB4474419 detected."
)

"%SYS32%\wmic.exe" qfe get HotFixID 2>nul | findstr /i "KB4490628" >nul 2>&1
if errorlevel 1 (
  set "KB_MISSING=1"
  call :log "WARNING: KB4490628 not detected. This servicing stack update is often required before KB4474419 can install."
) else (
  call :log "KB4490628 detected."
)

if "%KB_MISSING%"=="1" (
  call :log "         Guidance: docs/windows7-driver-troubleshooting.md#issue-missing-kb3033929-sha-256-signature-support"
)

exit /b 0

:load_signing_policy
set "SIGNING_POLICY=test"
if defined GT_SIGNING_POLICY set "SIGNING_POLICY=%GT_SIGNING_POLICY%"

if /i not "%SIGNING_POLICY%"=="test" if /i not "%SIGNING_POLICY%"=="production" if /i not "%SIGNING_POLICY%"=="none" (
  call :log "WARNING: Unknown signing_policy=%SIGNING_POLICY% (defaulting to test)."
  set "SIGNING_POLICY=test"
)

rem Explicit CLI overrides always win over the manifest/default.
if defined ARG_FORCE_SIGNING_POLICY (
  set "SIGNING_POLICY=%ARG_FORCE_SIGNING_POLICY%"
)

call :log "Effective signing_policy: %SIGNING_POLICY%"
exit /b 0

:warn_if_unexpected_certs
setlocal EnableDelayedExpansion
if /i "%SIGNING_POLICY%"=="test" (
  endlocal & exit /b 0
)

set "CERT_DIR=%SCRIPT_DIR%certs"
if not exist "!CERT_DIR!" (
  endlocal & exit /b 0
)

set "DID_WARN=0"

for %%F in ("!CERT_DIR!\*.cer") do (
  if exist "%%~fF" (
    if "!DID_WARN!"=="0" (
      call :log ""
      call :log "WARNING: signing_policy=%SIGNING_POLICY% but certificate file(s) were found under !CERT_DIR!."
      call :log "         Production/none Guest Tools media should NOT ship certificates."
      if "%ARG_INSTALL_CERTS%"=="1" (
        call :log "         NOTE: /installcerts was specified; setup will attempt to install them anyway."
      ) else (
        call :log "         They will be ignored (certificate installation is disabled by policy)."
      )
      set "DID_WARN=1"
    )
    call :log "         - %%~nxF"
  )
)

for %%F in ("!CERT_DIR!\*.crt") do (
  if exist "%%~fF" (
    if "!DID_WARN!"=="0" (
      call :log ""
      call :log "WARNING: signing_policy=%SIGNING_POLICY% but certificate file(s) were found under !CERT_DIR!."
      call :log "         Production/none Guest Tools media should NOT ship certificates."
      if "%ARG_INSTALL_CERTS%"=="1" (
        call :log "         NOTE: /installcerts was specified; setup will attempt to install them anyway."
      ) else (
        call :log "         They will be ignored (certificate installation is disabled by policy)."
      )
      set "DID_WARN=1"
    )
    call :log "         - %%~nxF"
  )
)

for %%F in ("!CERT_DIR!\*.p7b") do (
  if exist "%%~fF" (
    if "!DID_WARN!"=="0" (
      call :log ""
      call :log "WARNING: signing_policy=%SIGNING_POLICY% but certificate file(s) were found under !CERT_DIR!."
      call :log "         Production/none Guest Tools media should NOT ship certificates."
      if "%ARG_INSTALL_CERTS%"=="1" (
        call :log "         NOTE: /installcerts was specified; setup will attempt to install them anyway."
      ) else (
        call :log "         They will be ignored (certificate installation is disabled by policy)."
      )
      set "DID_WARN=1"
    )
    call :log "         - %%~nxF"
  )
)

endlocal & exit /b 0

:validate_manifest_signing_policy
call :log ""
call :log "Validating manifest signing_policy parsing..."

if not defined GT_MANIFEST exit /b 0

if not "%GT_PARSED_SIGNING_POLICY%"=="1" (
  call :log "ERROR: manifest.json found but signing_policy could not be parsed: %GT_MANIFEST%"
  call :log "       Ensure manifest.json includes a 'signing_policy' field (test|production|none)."
  exit /b 1
)

if /i not "%GT_SIGNING_POLICY%"=="test" if /i not "%GT_SIGNING_POLICY%"=="production" if /i not "%GT_SIGNING_POLICY%"=="none" (
  call :log "ERROR: Unsupported signing_policy in manifest.json: %GT_SIGNING_POLICY%"
  call :log "       Expected: test, production, or none."
  exit /b 1
)

set "EXPECT_CERTS=0"
if /i "%GT_SIGNING_POLICY%"=="test" set "EXPECT_CERTS=1"

if "%GT_PARSED_CERTS_REQUIRED%"=="1" if not "%GT_CERTS_REQUIRED%"=="%EXPECT_CERTS%" (
  call :log "ERROR: manifest.json certs_required=%GT_CERTS_REQUIRED% is inconsistent with signing_policy=%GT_SIGNING_POLICY%."
  exit /b 1
)

if "%GT_PARSED_CERTS_REQUIRED%"=="0" (
  call :log "WARNING: Could not parse certs_required from manifest.json; using derived value (certs_required=%GT_CERTS_REQUIRED%)."
)

call :log "OK: manifest signing_policy=%GT_SIGNING_POLICY% (certs_required=%GT_CERTS_REQUIRED%)."
exit /b 0

:validate_cert_payload
set "CERT_DIR=%SCRIPT_DIR%certs"
set "CERTS_REQUIRED=0"
if /i "%SIGNING_POLICY%"=="test" set "CERTS_REQUIRED=1"
call :log ""
call :log "Validating certificate files under %CERT_DIR% (signing_policy=%SIGNING_POLICY%)..."

if not exist "%CERT_DIR%" (
  if "%CERTS_REQUIRED%"=="1" (
    call :log "ERROR: Certificate directory not found: %CERT_DIR% (signing_policy=%SIGNING_POLICY%)."
    call :log "       Expected at least one: *.cer, *.crt, and/or *.p7b"
    exit /b %EC_CERTS_MISSING%
  )
  call :log "OK: Certificate directory not found; signing_policy=%SIGNING_POLICY% does not require certificates."
  exit /b 0
)

set "FOUND_CERT=0"
for %%F in ("%CERT_DIR%\*.cer") do if exist "%%~fF" set "FOUND_CERT=1"
for %%F in ("%CERT_DIR%\*.crt") do if exist "%%~fF" set "FOUND_CERT=1"
for %%F in ("%CERT_DIR%\*.p7b") do if exist "%%~fF" set "FOUND_CERT=1"

if "%FOUND_CERT%"=="0" (
  if "%CERTS_REQUIRED%"=="1" (
    call :log "ERROR: No certificates found under %CERT_DIR% (expected *.cer/*.crt and/or *.p7b)."
    call :log "       signing_policy=%SIGNING_POLICY% requires shipping certificate files."
    exit /b %EC_CERTS_MISSING%
  )
  call :log "OK: No certificate files found; signing_policy=%SIGNING_POLICY% does not require them."
  exit /b 0
)

call :log "OK: Found certificate file(s):"
for %%F in ("%CERT_DIR%\*.cer") do if exist "%%~fF" call :log "  - %%~nxF"
for %%F in ("%CERT_DIR%\*.crt") do if exist "%%~fF" call :log "  - %%~nxF"
for %%F in ("%CERT_DIR%\*.p7b") do if exist "%%~fF" call :log "  - %%~nxF"

if "%CERTS_REQUIRED%"=="0" (
  call :log "WARNING: signing_policy=%SIGNING_POLICY% but certificate file(s) were found under %CERT_DIR%."
  call :log "         Production/none Guest Tools media should NOT ship certificates."
)

exit /b 0

:install_certs
set "CERT_DIR=%SCRIPT_DIR%certs"
set "CERTS_REQUIRED=0"
if /i "%SIGNING_POLICY%"=="test" set "CERTS_REQUIRED=1"
call :log ""

rem In production/none mode, never import certificates by default. Production media should not ship cert files.
if /i not "%SIGNING_POLICY%"=="test" if not "%ARG_INSTALL_CERTS%"=="1" (
  call :log "signing_policy=%SIGNING_POLICY%: skipping certificate installation by policy."
  exit /b 0
)

if /i not "%SIGNING_POLICY%"=="test" if "%ARG_INSTALL_CERTS%"=="1" (
  call :log "WARNING: /installcerts specified; forcing certificate installation even though signing_policy=%SIGNING_POLICY%."
)

call :log "Checking certificate files under %CERT_DIR% (signing_policy=%SIGNING_POLICY%)..."

if not exist "%CERT_DIR%" (
  if "%CERTS_REQUIRED%"=="1" (
    call :log "ERROR: Certificate directory not found: %CERT_DIR% (signing_policy=%SIGNING_POLICY%)."
    call :log "       Expected at least one: *.cer, *.crt, and/or *.p7b"
    exit /b %EC_CERTS_MISSING%
  )
  if "%ARG_INSTALL_CERTS%"=="1" (
    call :log "WARNING: /installcerts requested but certificate directory not found: %CERT_DIR%."
  ) else (
    call :log "INFO: Certificate directory not found; signing_policy=%SIGNING_POLICY% does not require certificates; skipping certificate installation."
  )
  exit /b 0
)

set "FOUND_CERT=0"

for %%F in ("%CERT_DIR%\*.cer") do (
  if exist "%%~fF" (
    set "FOUND_CERT=1"
    call :install_one_cert "%%~fF" || exit /b 1
  )
)

for %%F in ("%CERT_DIR%\*.crt") do (
  if exist "%%~fF" (
    set "FOUND_CERT=1"
    call :install_one_cert "%%~fF" || exit /b 1
  )
)

for %%F in ("%CERT_DIR%\*.p7b") do (
  if exist "%%~fF" (
    set "FOUND_CERT=1"
    call :install_one_cert "%%~fF" || exit /b 1
  )
)

if "%FOUND_CERT%"=="0" (
  if "%CERTS_REQUIRED%"=="1" (
    call :log "ERROR: No certificates found under %CERT_DIR% (expected *.cer/*.crt and/or *.p7b)."
    call :log "       signing_policy=%SIGNING_POLICY% requires shipping certificate files."
    exit /b %EC_CERTS_MISSING%
  )
  if "%ARG_INSTALL_CERTS%"=="1" (
    call :log "WARNING: /installcerts requested but no certificate files found (no *.cer/*.crt/*.p7b)."
  ) else (
    call :log "INFO: No certificate files found (no *.cer/*.crt/*.p7b). signing_policy=%SIGNING_POLICY% does not require them; skipping certificate installation."
  )
  exit /b 0
)

exit /b 0

:install_one_cert
set "CERT_FILE=%~1"
call :log "  - %CERT_FILE%"

rem Record thumbprint(s) for uninstall where possible.
if /i "%~x1"==".cer" (
  call :record_cert_thumbprint "%CERT_FILE%"
)
if /i "%~x1"==".crt" (
  call :record_cert_thumbprint "%CERT_FILE%"
)
if /i "%~x1"==".p7b" (
  call :record_cert_thumbprint "%CERT_FILE%"
)

"%SYS32%\certutil.exe" -addstore -f Root "%CERT_FILE%" >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: certutil failed adding to Root: %CERT_FILE%"
  exit /b 1
)

"%SYS32%\certutil.exe" -addstore -f TrustedPublisher "%CERT_FILE%" >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: certutil failed adding to TrustedPublisher: %CERT_FILE%"
  exit /b 1
)

exit /b 0

:record_cert_thumbprint
set "CERT_FILE=%~1"
set "DUMP_FILE=%TEMP%\aerogt_certdump_%RANDOM%.txt"

"%SYS32%\certutil.exe" -dump "%CERT_FILE%" >"%DUMP_FILE%" 2>&1
if errorlevel 1 (
  del /q "%DUMP_FILE%" >nul 2>&1
  exit /b 0
)

rem certutil -dump prints one "Cert Hash(sha1)" for .cer/.crt, but can print many for .p7b.
for /f "tokens=2 delims=:" %%H in ('findstr /i "Cert Hash(sha1)" "%DUMP_FILE%"') do (
  set "RAW_THUMB=%%H"
  call :record_one_thumbprint "!RAW_THUMB!"
)
del /q "%DUMP_FILE%" >nul 2>&1

exit /b 0

:record_one_thumbprint
set "THUMB=%~1"
if not defined THUMB exit /b 0

rem Trim leading spaces and remove embedded spaces.
for /f "tokens=* delims= " %%T in ("%THUMB%") do set "THUMB=%%T"
set "THUMB=%THUMB: =%"

if not defined THUMB exit /b 0

if not exist "%CERT_LIST%" (
  >"%CERT_LIST%" echo %THUMB%
  exit /b 0
)

findstr /i /x "%THUMB%" "%CERT_LIST%" >nul 2>&1
if errorlevel 1 >>"%CERT_LIST%" echo %THUMB%
exit /b 0

:maybe_enable_testsigning
if /i not "%OS_ARCH%"=="amd64" exit /b 0
if "%ARG_FORCE_TESTSIGN%"=="1" if "%ARG_FORCE_NOINTEGRITY%"=="1" (
  call :log ""
  call :log "ERROR: /testsigning and /nointegritychecks cannot be used together."
  exit /b 1
)

set "NOINTEGRITY=0"
for /f "tokens=1,2" %%A in ('"%SYS32%\bcdedit.exe" /enum {current} ^| findstr /i "nointegritychecks"') do (
  if /i "%%B"=="Yes" set "NOINTEGRITY=1"
)

set "TESTSIGNING=0"
for /f "tokens=1,2" %%A in ('"%SYS32%\bcdedit.exe" /enum {current} ^| findstr /i "testsigning"') do (
  if /i "%%B"=="Yes" set "TESTSIGNING=1"
)

call :log ""
call :log "Windows 7 x64 detected. Effective signing_policy=%SIGNING_POLICY%"

rem Explicit /nointegritychecks always wins (NOT RECOMMENDED).
if "%ARG_FORCE_NOINTEGRITY%"=="1" (
  call :log "Kernel driver signature enforcement is strict. /nointegritychecks requested (NOT RECOMMENDED)."
  if "%NOINTEGRITY%"=="1" (
    call :log "nointegritychecks is already enabled."
    exit /b 0
  )

  call :log "Enabling nointegritychecks via bcdedit (NOT RECOMMENDED)..."
  "%SYS32%\bcdedit.exe" /set nointegritychecks on >>"%LOG%" 2>&1
  if errorlevel 1 (
    call :log "ERROR: Failed to enable nointegritychecks."
    call :log "You may need to run this manually and reboot:"
    call :log "  bcdedit /set nointegritychecks on"
    exit /b 1
  )

  > "%STATE_NOINTEGRITY%" echo nointegritychecks enabled by Aero Guest Tools on %DATE% %TIME%
  set "CHANGED_NOINTEGRITY=1"
  set "REBOOT_REQUIRED=1"
  call :log "nointegritychecks enabled. A reboot is required before it takes effect."
  exit /b 0
)

rem If this media is not intended for test-signed drivers, don't prompt to enable Test Signing
rem unless explicitly requested via /testsigning.
if /i not "%SIGNING_POLICY%"=="test" if not "%ARG_FORCE_TESTSIGN%"=="1" (
  call :log "signing_policy=%SIGNING_POLICY%: leaving Test Signing / nointegritychecks unchanged."
  if "%TESTSIGNING%"=="1" (
    call :log "INFO: Test Signing is currently enabled, but is not required by this Guest Tools build."
  )
  if "%NOINTEGRITY%"=="1" (
    call :log "WARNING: nointegritychecks is enabled (NOT RECOMMENDED)."
  )
  exit /b 0
)

rem Default: signing_policy=test (legacy behavior).
if "%ARG_SKIP_TESTSIGN%"=="1" (
  call :log "Skipping test signing changes (/notestsigning)."
  exit /b 0
)

call :log "Kernel driver signature enforcement is strict."
call :log "If Aero drivers are test-signed/custom-signed, enable Test Signing mode."
call :log "Alternative (less safe): disable signature checks entirely (nointegritychecks)."

if "%TESTSIGNING%"=="1" (
  call :log "Test Signing is already enabled."
  exit /b 0
)

if "%NOINTEGRITY%"=="1" (
  if "%ARG_FORCE_TESTSIGN%"=="1" (
    call :log "nointegritychecks is enabled, but /testsigning was requested; enabling Test Signing anyway..."
  ) else (
    call :log "nointegritychecks is already enabled. Test Signing is not required."
    exit /b 0
  )
)

set "DO_ENABLE=0"
if "%ARG_FORCE_TESTSIGN%"=="1" (
  set "DO_ENABLE=1"
) else (
  choice /c YN /n /m "Enable Test Signing now (recommended for test-signed drivers)? [Y/N] "
  if errorlevel 2 set "DO_ENABLE=0"
  if errorlevel 1 set "DO_ENABLE=1"
)

if "%DO_ENABLE%"=="0" (
  call :log "Test Signing was not enabled."
  exit /b 0
)

call :log "Enabling Test Signing via bcdedit..."
"%SYS32%\bcdedit.exe" /set testsigning on >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "WARNING: Failed to enable Test Signing (bcdedit /set testsigning on)."
  call :log "You may need to run this manually and reboot:"
  call :log "  bcdedit /set testsigning on"
  call :log "Alternative (less safe):"
  call :log "  bcdedit /set nointegritychecks on"
  if "%ARG_FORCE_TESTSIGN%"=="1" exit /b 1
  exit /b 0
)

> "%STATE_TESTSIGN%" echo TestSigning enabled by Aero Guest Tools on %DATE% %TIME%
set "CHANGED_TESTSIGNING=1"
set "REBOOT_REQUIRED=1"
call :log "Test Signing enabled. A reboot is required before it takes effect."
exit /b 0

:stage_all_drivers
call :log ""
call :log "Staging Aero drivers from %DRIVER_DIR% ..."
if "%ARG_STAGE_ONLY%"=="1" (
  call :log "Driver install attempts are disabled (/stageonly)."
)

set "INF_FOUND=0"
for /r "%DRIVER_DIR%" %%F in (*.inf) do (
  set "INF_FOUND=1"
  call :stage_one_inf "%%~fF" || exit /b 1
)

if "%INF_FOUND%"=="0" (
  call :log "ERROR: No .inf files found under %DRIVER_DIR%."
  exit /b 1
)

exit /b 0

:stage_one_inf
set "INF=%~1"
call :log ""
call :log "INF: %INF%"

set "OUT=%TEMP%\aerogt_pnputil_add_%RANDOM%.txt"
"%SYS32%\pnputil.exe" -a "%INF%" >"%OUT%" 2>&1
type "%OUT%" >>"%LOG%"
set "RC=%ERRORLEVEL%"

set "PUBLISHED="
call :extract_first_pnputil_published_inf "%OUT%" PUBLISHED

if defined PUBLISHED (
  call :log "pnputil published name: !PUBLISHED!"
) else (
  call :log "pnputil published name: (none detected)"
)

rem pnputil on Windows 7 is not consistent about exit codes for idempotent "already imported" cases.
rem Locale-independent handling: treat a non-zero exit code as success if pnputil still produced an OEM*.INF name.
if not "%RC%"=="0" (
  if defined PUBLISHED (
    call :log "pnputil returned %RC%, but an OEM INF name was detected; treating as success."
    set "RC=0"
  )
)

del /q "%OUT%" >nul 2>&1

if not "%RC%"=="0" (
  call :log "ERROR: pnputil -a failed for %INF% (exit code %RC%)."
  exit /b 1
)

if defined PUBLISHED (
  call :record_published_inf "!PUBLISHED!"
)

if "%ARG_STAGE_ONLY%"=="1" exit /b 0

set "OUT=%TEMP%\aerogt_pnputil_install_%RANDOM%.txt"
"%SYS32%\pnputil.exe" -i -a "%INF%" >"%OUT%" 2>&1
type "%OUT%" >>"%LOG%"
set "RC=%ERRORLEVEL%"
del /q "%OUT%" >nul 2>&1

if not "%RC%"=="0" (
  call :log "WARNING: pnputil -i -a returned %RC% for %INF%."
  call :log "         This is expected if no matching device is currently present."
)

exit /b 0

:extract_first_pnputil_published_inf
rem Extract the first published driver package name reported by pnputil by scanning for an
rem OEM*.INF pattern. This is more robust than matching locale-specific "Published name"
rem output strings.
rem Usage: call :extract_first_pnputil_published_inf <output_file> <out_var_name>
setlocal EnableDelayedExpansion
set "OUT_FILE=%~1"
set "MATCH="
if not exist "!OUT_FILE!" (
  endlocal & set "%~2=" & exit /b 0
)

set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "!PWSH!" set "PWSH=powershell.exe"
set "AEROGT_PNPUTIL_OUT=!OUT_FILE!"

rem We only need to extract an ASCII OEM*.INF identifier. Decode as ASCII and strip NULs so
rem UTF-16LE/BE redirected output can still be searched without relying on locale strings.
for /f "usebackq delims=" %%M in (`"!PWSH!" -NoProfile -ExecutionPolicy Bypass -Command "$p=$env:AEROGT_PNPUTIL_OUT; try{ $bytes=[System.IO.File]::ReadAllBytes($p) }catch{ exit 1 }; $text=[System.Text.Encoding]::ASCII.GetString($bytes); $text=$text.Replace([char]0,''); $m=[regex]::Match($text,'(?i)oem[0-9]+\.inf'); if($m.Success){ $m.Value.Trim() }"`) do (
  set "MATCH=%%M"
  goto :extract_first_pnputil_published_inf_done
)

:extract_first_pnputil_published_inf_done
if defined MATCH (
  echo(!MATCH!| "%SYS32%\findstr.exe" /i /r /c:"^oem[0-9][0-9]*[.]inf$" >nul 2>&1
  if errorlevel 1 set "MATCH="
)
endlocal & set "%~2=%MATCH%" & exit /b 0

:record_published_inf
set "PUB=%~1"
if not defined PUB exit /b 0

if not exist "%PKG_LIST%" (
  >"%PKG_LIST%" echo %PUB%
  exit /b 0
)

findstr /i /x "%PUB%" "%PKG_LIST%" >nul 2>&1
if errorlevel 1 >>"%PKG_LIST%" echo %PUB%
exit /b 0

:validate_storage_service_infs
call :log ""
call :log "Validating virtio-blk storage service name against driver INF packages..."
call :log "Driver directory: %DRIVER_DIR%"
call :log "Config file: %SCRIPT_DIR%config\devices.cmd"

set "TARGET_SVC=%AERO_VIRTIO_BLK_SERVICE%"
set "SCAN_LIST=%TEMP%\aerogt_infscan_%RANDOM%.txt"
del /q "%SCAN_LIST%" >nul 2>&1

set "INF_COUNT=0"
set "FOUND_MATCH=0"
set "MATCH_INF="

for /r "%DRIVER_DIR%" %%F in (*.inf) do (
  set /a INF_COUNT+=1
  rem Quote paths to avoid breaking the FOR (...) block on common characters like spaces, parentheses, and &.
  >>"%SCAN_LIST%" echo "%%~fF"
  call :inf_contains_addservice_findstr "%%~fF" "%TARGET_SVC%"
  if not errorlevel 1 (
    set "FOUND_MATCH=1"
    if not defined MATCH_INF set "MATCH_INF=%%~fF"
  )
)

if "%INF_COUNT%"=="0" (
  call :log "ERROR: No .inf files found under %DRIVER_DIR% for validation."
  del /q "%SCAN_LIST%" >nul 2>&1
  exit /b 1
)

if "%FOUND_MATCH%"=="1" (
  call :log "OK: Found AddService=%TARGET_SVC% in: %MATCH_INF%"
  del /q "%SCAN_LIST%" >nul 2>&1
  exit /b 0
)

rem No ASCII/UTF-8 match found. Some driver bundles ship UTF-16 INFs, which `findstr`
rem may not be able to scan. Do one PowerShell pass over the scan list (BOM-aware,
rem plus a UTF-16-without-BOM heuristic) before failing with EC_STORAGE_SERVICE_MISMATCH.
set "PWSH_MATCH_INF="
call :inf_find_addservice_in_scan_list_powershell "%SCAN_LIST%" "%TARGET_SVC%"
if not errorlevel 1 (
  set "FOUND_MATCH=1"
  set "MATCH_INF=!PWSH_MATCH_INF!"
  call :log "OK: Found AddService=%TARGET_SVC% in: !MATCH_INF!"
  del /q "%SCAN_LIST%" >nul 2>&1
  exit /b 0
)

call :log "ERROR: Configured AERO_VIRTIO_BLK_SERVICE=%TARGET_SVC% does not match any driver INF AddService name."
call :log "Expected to find an INF line (case-insensitive) like:"
call :log "  AddService = %TARGET_SVC%, ..."
call :log "  AddService = ^"%TARGET_SVC%^", ..."
call :log ""
call :log "If you are intentionally using Guest Tools media that does not include the virtio-blk storage driver (e.g. AeroGPU-only),"
call :log "re-run setup.cmd with /skipstorage to continue WITHOUT boot-critical storage pre-seeding."
call :log "WARNING: When /skipstorage is used, do NOT switch the boot disk from AHCI -> virtio-blk (0x7B risk)."
call :log "Scanned INF files:"
for /f "usebackq delims=" %%I in ("%SCAN_LIST%") do call :log "  - %%~I"
del /q "%SCAN_LIST%" >nul 2>&1
exit /b %EC_STORAGE_SERVICE_MISMATCH%

:inf_contains_addservice_findstr
setlocal EnableDelayedExpansion
set "INF_FILE=%~1"
set "TARGET=%~2"

for /f "delims=" %%L in ('"%SYS32%\findstr.exe" /i /c:"AddService" "%INF_FILE%" 2^>nul') do (
  set "LINE=%%L"
  rem Ensure the line doesn't contain embedded quotes that would break subsequent parsing.
  set "LINE=!LINE:"=!"
  set "LEFT="
  set "RIGHT="
  for /f "tokens=1,* delims==" %%A in ("!LINE!") do (
    set "LEFT=%%A"
    set "RIGHT=%%B"
  )
  if not defined RIGHT (
    rem Not an AddService assignment (e.g. a section name); ignore.
  ) else (
    rem Normalize whitespace on the left-hand side (tabs/spaces) without requiring
    rem literal tab characters in the script.
    for /f "tokens=1" %%X in ("!LEFT!") do set "LEFT=%%X"
    if /i "!LEFT!"=="AddService" (
      set "REST=!RIGHT!"
      set "REST=!REST:"=!"
      set "SVC="
      for /f "tokens=1 delims=," %%S in ("!REST!") do set "SVC=%%S"
      for /f "tokens=1 delims=;" %%S in ("!SVC!") do set "SVC=%%S"
      for /f "tokens=1" %%S in ("!SVC!") do set "SVC=%%S"
      if /i "!SVC!"=="!TARGET!" (
        endlocal & exit /b 0
      )
    )
  )
)

endlocal & exit /b 1

:inf_find_addservice_in_scan_list_powershell
setlocal EnableDelayedExpansion
set "SCAN_LIST=%~1"
set "TARGET=%~2"
set "MATCH_INF="

set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PWSH%" set "PWSH=powershell.exe"

set "AEROGT_INF_SCAN_LIST=%SCAN_LIST%"
set "AEROGT_INF_TARGET=%TARGET%"

for /f "usebackq delims=" %%M in (`"%PWSH%" -NoProfile -ExecutionPolicy Bypass -Command "$list=$env:AEROGT_INF_SCAN_LIST; $target=$env:AEROGT_INF_TARGET; try{ $enc=[Console]::OutputEncoding; $bytes=[System.IO.File]::ReadAllBytes($list); $text=$enc.GetString($bytes); $paths=$text -split '\r\n|\n|\r' }catch{ exit 1 }; foreach($p in $paths){ $inf=$p.Trim(); if($inf.Length -ge 2 -and $inf[0] -eq [char]34 -and $inf[$inf.Length-1] -eq [char]34){ $inf=$inf.Substring(1,$inf.Length-2) }; try{ $b=[System.IO.File]::ReadAllBytes($inf) }catch{ continue }; $text=$null; if($b.Length -ge 3 -and $b[0] -eq 0xEF -and $b[1] -eq 0xBB -and $b[2] -eq 0xBF){ $text=[System.Text.Encoding]::UTF8.GetString($b,3,$b.Length-3) } elseif($b.Length -ge 2 -and $b[0] -eq 0xFF -and $b[1] -eq 0xFE){ $text=[System.Text.Encoding]::Unicode.GetString($b,2,$b.Length-2) } elseif($b.Length -ge 2 -and $b[0] -eq 0xFE -and $b[1] -eq 0xFF){ $text=[System.Text.Encoding]::BigEndianUnicode.GetString($b,2,$b.Length-2) } elseif($b.Length -ge 4 -and ($b.Length -band 1) -eq 0){ $nul=0; $evenNul=0; $oddNul=0; for($i=0; $i -lt $b.Length; $i++){ if($b[$i] -eq 0){ $nul++; if(($i -band 1) -eq 0){ $evenNul++ } else { $oddNul++ } } }; $nulRatio=$nul/[double]$b.Length; if($nulRatio -ge 0.3){ $half=$b.Length/2; $evenRatio=$evenNul/[double]$half; $oddRatio=$oddNul/[double]$half; $guess=0; if($oddRatio -gt $evenRatio + 0.2){ $guess=1 } elseif($evenRatio -gt $oddRatio + 0.2){ $guess=2 }; $le=[System.Text.Encoding]::Unicode.GetString($b); $be=[System.Text.Encoding]::BigEndianUnicode.GetString($b); $leRep=0; $leNulChar=0; foreach($ch in $le.ToCharArray()){ if($ch -eq [char]0xFFFD){ $leRep++ } elseif($ch -eq [char]0){ $leNulChar++ } }; $beRep=0; $beNulChar=0; foreach($ch in $be.ToCharArray()){ if($ch -eq [char]0xFFFD){ $beRep++ } elseif($ch -eq [char]0){ $beNulChar++ } }; if($leRep -lt $beRep -or ($leRep -eq $beRep -and $leNulChar -lt $beNulChar)){ $text=$le } elseif($beRep -lt $leRep -or ($beRep -eq $leRep -and $beNulChar -lt $leNulChar)){ $text=$be } else { if($guess -eq 2){ $text=$be } else { $text=$le } } } }; if($text -eq $null){ $text=[System.Text.Encoding]::UTF8.GetString($b) }; foreach($line in ($text -split '\r\n|\n|\r')){ if($line.Length -gt 0 -and $line[0] -eq [char]0xFEFF){ $line=$line.Substring(1) }; $noComment=$line.Split(';')[0]; $noComment=$noComment.Replace([char]34,''); if($noComment -match '^\s*AddService\s*=\s*([^,;\s]+)'){ if([string]::Equals($matches[1],$target,[System.StringComparison]::OrdinalIgnoreCase)){ [Console]::WriteLine($inf); exit 0 } } } }; exit 1" 2^>nul`) do (
  set "MATCH_INF=%%M"
)

if defined MATCH_INF (
  endlocal & set "PWSH_MATCH_INF=%MATCH_INF%" & exit /b 0
)

endlocal & set "PWSH_MATCH_INF=" & exit /b 1

:inf_contains_addservice
setlocal EnableDelayedExpansion
set "INF_FILE=%~1"
set "TARGET=%~2"

set "HAD_FINDSTR=0"
for /f "delims=" %%L in ('"%SYS32%\findstr.exe" /i /c:"AddService" "%INF_FILE%" 2^>nul') do (
  set "HAD_FINDSTR=1"
  set "LINE=%%L"
  rem Ensure the line doesn't contain embedded quotes that would break subsequent parsing.
  set "LINE=!LINE:"=!"
  set "LEFT="
  set "RIGHT="
  for /f "tokens=1,* delims==" %%A in ("!LINE!") do (
    set "LEFT=%%A"
    set "RIGHT=%%B"
  )
  if not defined RIGHT (
    rem Not an AddService assignment (e.g. a section name); ignore.
  ) else (
    rem Normalize whitespace on the left-hand side (tabs/spaces) without requiring
    rem literal tab characters in the script.
    for /f "tokens=1" %%X in ("!LEFT!") do set "LEFT=%%X"
    if /i "!LEFT!"=="AddService" (
      set "REST=!RIGHT!"
      set "REST=!REST:"=!"
      set "SVC="
      for /f "tokens=1 delims=," %%S in ("!REST!") do set "SVC=%%S"
      for /f "tokens=1 delims=;" %%S in ("!SVC!") do set "SVC=%%S"
      for /f "tokens=1" %%S in ("!SVC!") do set "SVC=%%S"
      if /i "!SVC!"=="!TARGET!" (
        endlocal & exit /b 0
      )
    )
  )
)

rem If findstr produced any AddService-containing lines but none matched the target
rem service name, there's no need for a slower/encoding-aware fallback.
if "%HAD_FINDSTR%"=="1" (
  endlocal & exit /b 1
)

rem Fallback for UTF-16 (including some BOM-less files): findstr/cmd parsing can yield
rem no output at all. Use PowerShell 2.0 + ReadAllBytes(), decoding with:
rem - BOM-aware detection (UTF-8/UTF-16LE/UTF-16BE), and
rem - a UTF-16-without-BOM heuristic (many 0x00 bytes).
set "PWSH=%SYS32%\WindowsPowerShell\v1.0\powershell.exe"
if not exist "%PWSH%" set "PWSH=powershell.exe"
set "AEROGT_INF_FILE=%INF_FILE%"
set "AEROGT_INF_TARGET=%TARGET%"
"%PWSH%" -NoProfile -ExecutionPolicy Bypass -Command "$inf=$env:AEROGT_INF_FILE; $target=$env:AEROGT_INF_TARGET; try{ $b=[System.IO.File]::ReadAllBytes($inf) }catch{ exit 1 }; $text=$null; if($b.Length -ge 3 -and $b[0] -eq 0xEF -and $b[1] -eq 0xBB -and $b[2] -eq 0xBF){ $text=[System.Text.Encoding]::UTF8.GetString($b,3,$b.Length-3) } elseif($b.Length -ge 2 -and $b[0] -eq 0xFF -and $b[1] -eq 0xFE){ $text=[System.Text.Encoding]::Unicode.GetString($b,2,$b.Length-2) } elseif($b.Length -ge 2 -and $b[0] -eq 0xFE -and $b[1] -eq 0xFF){ $text=[System.Text.Encoding]::BigEndianUnicode.GetString($b,2,$b.Length-2) } elseif($b.Length -ge 4 -and ($b.Length -band 1) -eq 0){ $nul=0; $evenNul=0; $oddNul=0; for($i=0; $i -lt $b.Length; $i++){ if($b[$i] -eq 0){ $nul++; if(($i -band 1) -eq 0){ $evenNul++ } else { $oddNul++ } } }; $nulRatio=$nul/[double]$b.Length; if($nulRatio -ge 0.3){ $half=$b.Length/2; $evenRatio=$evenNul/[double]$half; $oddRatio=$oddNul/[double]$half; $guess=0; if($oddRatio -gt $evenRatio + 0.2){ $guess=1 } elseif($evenRatio -gt $oddRatio + 0.2){ $guess=2 }; $le=[System.Text.Encoding]::Unicode.GetString($b); $be=[System.Text.Encoding]::BigEndianUnicode.GetString($b); $leRep=0; $leNulChar=0; foreach($ch in $le.ToCharArray()){ if($ch -eq [char]0xFFFD){ $leRep++ } elseif($ch -eq [char]0){ $leNulChar++ } }; $beRep=0; $beNulChar=0; foreach($ch in $be.ToCharArray()){ if($ch -eq [char]0xFFFD){ $beRep++ } elseif($ch -eq [char]0){ $beNulChar++ } }; if($leRep -lt $beRep -or ($leRep -eq $beRep -and $leNulChar -lt $beNulChar)){ $text=$le } elseif($beRep -lt $leRep -or ($beRep -eq $leRep -and $beNulChar -lt $leNulChar)){ $text=$be } else { if($guess -eq 2){ $text=$be } else { $text=$le } } } }; if($text -eq $null){ $text=[System.Text.Encoding]::UTF8.GetString($b) }; foreach($line in ($text -split '\r\n|\n|\r')){ if($line.Length -gt 0 -and $line[0] -eq [char]0xFEFF){ $line=$line.Substring(1) }; $noComment=$line.Split(';')[0]; $noComment=$noComment.Replace([char]34,''); if($noComment -match '^\s*AddService\s*=\s*([^,;\s]+)'){ if([string]::Equals($matches[1],$target,[System.StringComparison]::OrdinalIgnoreCase)){ exit 0 } } }; exit 1" >nul 2>&1
if not errorlevel 1 (
  endlocal & exit /b 0
)

endlocal & exit /b 1

:preseed_storage_boot
call :log ""
call :log "Preparing boot-critical virtio-blk storage plumbing..."

set "STOR_SERVICE=%AERO_VIRTIO_BLK_SERVICE%"
set "STOR_SYS=%AERO_VIRTIO_BLK_SYS%"
if not defined STOR_SYS set "STOR_SYS=%STOR_SERVICE%.sys"

call :log "Storage service: %STOR_SERVICE%"
call :log "Storage driver:  %STOR_SYS%"

rem Ensure the driver binary is in \System32\drivers for boot-start loading.
set "STOR_TARGET=%SYS32%\drivers\%STOR_SYS%"
if exist "%STOR_TARGET%" (
  call :log "Storage driver already present: %STOR_TARGET%"
) else (
  set "SYS_SOURCE="
  for /r "%DRIVER_DIR%" %%S in (%STOR_SYS%) do (
    if exist "%%~fS" set "SYS_SOURCE=%%~fS"
  )

  if defined SYS_SOURCE (
    call :log "Copying %STOR_SYS% to %SYS32%\\drivers ..."
    copy /y "%SYS_SOURCE%" "%STOR_TARGET%" >>"%LOG%" 2>&1
    if errorlevel 1 (
      call :log "ERROR: Failed to copy %STOR_SYS% to %STOR_TARGET%."
      exit /b 1
    )
  ) else (
    call :log "ERROR: Could not locate %STOR_SYS% under %DRIVER_DIR%, and it is not present in %STOR_TARGET%."
    call :log "       Refusing to continue because switching the boot disk to virtio-blk will likely BSOD (0x7B)."
    call :log ""
    call :log "If you are intentionally using Guest Tools media that does not include the virtio-blk storage driver (e.g. AeroGPU-only),"
    call :log "re-run setup.cmd with /skipstorage to continue WITHOUT boot-critical storage pre-seeding."
    call :log "WARNING: When /skipstorage is used, do NOT switch the boot disk from AHCI -> virtio-blk (0x7B risk)."
    exit /b 1
  )
)

rem Ensure the service exists and is BOOT_START.
set "SVC_KEY=HKLM\SYSTEM\CurrentControlSet\Services\%STOR_SERVICE%"
"%SYS32%\reg.exe" add "%SVC_KEY%" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to create/verify service key: %SVC_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Type /t REG_DWORD /d 1 /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set Type for service key: %SVC_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Start /t REG_DWORD /d 0 /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set Start for service key: %SVC_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%SVC_KEY%" /v ErrorControl /t REG_DWORD /d 1 /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set ErrorControl for service key: %SVC_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Group /t REG_SZ /d "SCSI miniport" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set Group for service key: %SVC_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%SVC_KEY%" /v ImagePath /t REG_EXPAND_SZ /d "system32\drivers\%STOR_SYS%" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set ImagePath for service key: %SVC_KEY%"
  exit /b 1
)

rem CriticalDeviceDatabase pre-seed: map PCI hardware IDs to the storage service.
set "CDD_BASE=HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase"
set "SCSIADAPTER_GUID={4D36E97B-E325-11CE-BFC1-08002BE10318}"

for %%H in (%AERO_VIRTIO_BLK_HWIDS%) do (
  call :add_cdd_keys "%%~H" "%STOR_SERVICE%" "%SCSIADAPTER_GUID%" || exit /b 1
)

exit /b 0

:skip_storage_preseed
call :log ""
call :log "Skipping boot-critical virtio-blk storage pre-seeding (/skipstorage)."
call :log "WARNING: Do NOT switch the boot disk from AHCI -> virtio-blk after this run."
call :log "         Windows may BSOD with 0x7B (INACCESSIBLE_BOOT_DEVICE) because storage registry/service keys were not written."
call :log "         Re-run setup.cmd without /skipstorage once virtio-blk drivers are available on the Guest Tools media."

> "%STATE_STORAGE_SKIPPED%" echo storage pre-seeding intentionally skipped by Aero Guest Tools on %DATE% %TIME%
if not exist "%STATE_STORAGE_SKIPPED%" (
  call :log "ERROR: Failed to write marker file: %STATE_STORAGE_SKIPPED%"
  exit /b 1
)
call :log "Wrote marker file: %STATE_STORAGE_SKIPPED%"
exit /b 0

:clear_storage_skip_marker
if exist "%STATE_STORAGE_SKIPPED%" (
  del /q "%STATE_STORAGE_SKIPPED%" >nul 2>&1
  if exist "%STATE_STORAGE_SKIPPED%" (
    call :log "WARNING: Failed to remove marker file: %STATE_STORAGE_SKIPPED%"
  )
)
exit /b 0

:add_cdd_keys
set "HWID=%~1"
set "SERVICE=%~2"
set "CLASSGUID=%~3"

set "KEYNAME=%HWID:\=#%"

call :add_one_cdd "%KEYNAME%" "%SERVICE%" "%CLASSGUID%" || exit /b 1
call :add_one_cdd "%KEYNAME%&CC_010000" "%SERVICE%" "%CLASSGUID%" || exit /b 1
call :add_one_cdd "%KEYNAME%&CC_0100" "%SERVICE%" "%CLASSGUID%" || exit /b 1
exit /b 0

:add_one_cdd
set "CDD_KEY=%CDD_BASE%\%~1"
set "SERVICE=%~2"
set "CLASSGUID=%~3"

"%SYS32%\reg.exe" add "%CDD_KEY%" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to create CriticalDeviceDatabase key: %CDD_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%CDD_KEY%" /v Service /t REG_SZ /d "%SERVICE%" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set Service for CriticalDeviceDatabase key: %CDD_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%CDD_KEY%" /v ClassGUID /t REG_SZ /d "%CLASSGUID%" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set ClassGUID for CriticalDeviceDatabase key: %CDD_KEY%"
  exit /b 1
)
"%SYS32%\reg.exe" add "%CDD_KEY%" /v Class /t REG_SZ /d "SCSIAdapter" /f >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "ERROR: Failed to set Class for CriticalDeviceDatabase key: %CDD_KEY%"
  exit /b 1
)
exit /b 0

:maybe_reboot
if "%ARG_NO_REBOOT%"=="1" exit /b 0

call :log ""
call :log "Reboot/shutdown is recommended before switching the VM's boot disk to virtio-blk."
if "%REBOOT_REQUIRED%"=="1" (
  call :log "A reboot is REQUIRED to apply boot configuration changes (test signing / nointegritychecks)."
)
call :log ""

choice /c RSN /n /m "Reboot (R), Shutdown (S), or No action (N)? "
set "CH=%ERRORLEVEL%"
if "%CH%"=="1" (
  call :log "Rebooting now..."
  "%SYS32%\shutdown.exe" /r /t 0 >>"%LOG%" 2>&1
  exit /b 0
)
if "%CH%"=="2" (
  call :log "Shutting down now..."
  "%SYS32%\shutdown.exe" /s /t 0 >>"%LOG%" 2>&1
  exit /b 0
)

call :log "No reboot/shutdown selected."
exit /b 0
