@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem Aero Guest Tools installer for Windows 7 SP1 (x86/x64).
rem Offline + built-in tooling only: certutil, pnputil, reg, bcdedit, shutdown.

rem Standard exit codes (stable for automation/scripted use).
set "EC_ADMIN_REQUIRED=10"
set "EC_DRIVER_DIR_MISSING=11"
set "EC_CERTS_MISSING=12"
set "EC_STORAGE_SERVICE_MISMATCH=13"

set "SCRIPT_DIR=%~dp0"

rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"

pushd "%SCRIPT_DIR%" >nul 2>&1
if errorlevel 1 (
  echo ERROR: Could not cd to "%SCRIPT_DIR%".
  exit /b 1
)

set "INSTALL_ROOT=C:\AeroGuestTools"
set "LOG=%INSTALL_ROOT%\install.log"
set "PKG_LIST=%INSTALL_ROOT%\installed-driver-packages.txt"
set "CERT_LIST=%INSTALL_ROOT%\installed-certs.txt"
set "STATE_TESTSIGN=%INSTALL_ROOT%\testsigning.enabled-by-aero.txt"
set "STATE_NOINTEGRITY=%INSTALL_ROOT%\nointegritychecks.enabled-by-aero.txt"
set "STATE_STORAGE_SKIPPED=%INSTALL_ROOT%\storage-preseed.skipped.txt"

set "ARG_FORCE=0"
set "ARG_STAGE_ONLY=0"
set "ARG_FORCE_TESTSIGN=0"
set "ARG_SKIP_TESTSIGN=0"
set "ARG_FORCE_NOINTEGRITY=0"
set "ARG_FORCE_SIGNING_POLICY="
set "ARG_NO_REBOOT=0"
set "ARG_SKIP_STORAGE=0"

set "SIGNING_POLICY=testsigning"

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
  if /i "%%~A"=="/testsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/forcetestsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/force-testsigning" set "ARG_FORCE_TESTSIGN=1"
  if /i "%%~A"=="/notestsigning" set "ARG_SKIP_TESTSIGN=1"
  if /i "%%~A"=="/no-testsigning" set "ARG_SKIP_TESTSIGN=1"
  if /i "%%~A"=="/nointegritychecks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/forcenointegritychecks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/no-integrity-checks" set "ARG_FORCE_NOINTEGRITY=1"
  if /i "%%~A"=="/forcesigningpolicy:none" set "ARG_FORCE_SIGNING_POLICY=none"
  if /i "%%~A"=="/forcesigningpolicy:testsigning" set "ARG_FORCE_SIGNING_POLICY=testsigning"
  if /i "%%~A"=="/forcesigningpolicy:nointegritychecks" set "ARG_FORCE_SIGNING_POLICY=nointegritychecks"
  if /i "%%~A"=="/noreboot" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/no-reboot" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/skipstorage" set "ARG_SKIP_STORAGE=1"
  if /i "%%~A"=="/skip-storage" set "ARG_SKIP_STORAGE=1"
)

rem /force implies fully non-interactive behavior.
if "%ARG_FORCE%"=="1" (
  set "ARG_NO_REBOOT=1"
)

call :require_admin_stdout
if errorlevel 1 (
  set "RC=%ERRORLEVEL%"
  popd >nul 2>&1
  exit /b %RC%
)
call :init_logging || goto :fail
call :log "Aero Guest Tools setup starting..."
call :log "Script dir: %SCRIPT_DIR%"
call :log "System tools: %SYS32%"
call :log "Logs: %LOG%"
call :log_manifest

call :require_admin || goto :fail
call :detect_arch || goto :fail
call :load_signing_policy
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
echo   /force, /quiet        Non-interactive: implies /noreboot; on x64 applies signing_policy without prompting
echo                        (use /forcesigningpolicy:none to keep boot policy unchanged)
echo   /stageonly           Only stage drivers into the Driver Store (no install attempts)
echo   /testsigning         Enable test signing on x64 without prompting (overrides manifest)
echo   /forcetestsigning    Same as /testsigning
echo   /notestsigning       Skip enabling test signing (x64)
echo   /nointegritychecks   Disable signature enforcement (x64; not recommended; overrides manifest)
echo   /forcenointegritychecks  Same as /nointegritychecks
echo   /forcesigningpolicy:none^|testsigning^|nointegritychecks
echo                      Override the signing policy read from manifest.json (if present)
echo   /noreboot            Do not prompt to reboot/shutdown at the end
echo   /skipstorage         Skip boot-critical virtio-blk storage pre-seeding (alias: /skip-storage; advanced; unsafe to switch boot disk to virtio-blk)
echo.
echo Logs are written to C:\AeroGuestTools\install.log
popd >nul 2>&1
exit /b 0

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
echo(%*
>>"%LOG%" echo(%*
exit /b 0

:require_admin_stdout
"%SYS32%\fsutil.exe" dirty query %SYSTEMDRIVE% >nul 2>&1
if errorlevel 1 (
  echo ERROR: Administrator privileges are required.
  echo Right-click setup.cmd and choose 'Run as administrator'.
  exit /b %EC_ADMIN_REQUIRED%
)
exit /b 0

:log_manifest
setlocal EnableDelayedExpansion

rem Optional: record which Guest Tools build produced the media (if provided).
set "MEDIA_ROOT="
for %%I in ("%SCRIPT_DIR%..") do set "MEDIA_ROOT=%%~fI"
set "MANIFEST=!MEDIA_ROOT!\manifest.json"
if not exist "!MANIFEST!" set "MANIFEST=%SCRIPT_DIR%manifest.json"
if not exist "!MANIFEST!" (
  endlocal & exit /b 0
)

set "GT_VERSION="
set "GT_BUILD_ID="
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
)

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

endlocal & exit /b 0

:log_summary
call :log ""
call :log "==================== Summary ===================="
call :log "OS architecture: %OS_ARCH%"
call :log "Storage service: %AERO_VIRTIO_BLK_SERVICE%"
call :log Seeded HWIDs: %AERO_VIRTIO_BLK_HWIDS%
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

rem /force skips prompts. Apply the effective signing policy unless the operator
rem explicitly disables/overrides it.
if /i "%SIGNING_POLICY%"=="none" (
  call :log "Force mode: signing_policy=none; leaving Test Signing / nointegritychecks unchanged."
  exit /b 0
)

if /i "%SIGNING_POLICY%"=="nointegritychecks" (
  if "%ARG_FORCE_NOINTEGRITY%"=="1" (
    call :log "Force mode: will enable nointegritychecks on x64 (explicit)."
    exit /b 0
  )
  set "ARG_FORCE_NOINTEGRITY=1"
  call :log "Force mode: will enable nointegritychecks on x64 (implied by signing_policy)."
  exit /b 0
)

rem Default: signing_policy=testsigning.
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
rem KB3033929 adds SHA-256 signature validation support to Windows 7.
rem If Aero's driver catalogs are SHA-256 signed and this update is missing,
rem Device Manager may report Code 52 (signature verification failure).
call :log ""
call :log "Checking for KB3033929 (SHA-256 signature support)..."

if /i "%OS_ARCH%"=="amd64" (
  if not exist "%SYS32%\wmic.exe" (
    call :log "WARNING: wmic.exe not found; cannot detect KB3033929."
    exit /b 0
  )

  "%SYS32%\wmic.exe" qfe get HotFixID 2>nul | findstr /i "KB3033929" >nul 2>&1
  if errorlevel 1 (
    call :log "WARNING: KB3033929 not detected. If Aero driver packages are SHA-256 signed, Windows 7 x64 may refuse to load them (Code 52)."
    call :log "         Install KB3033929 (offline) or use SHA-1 signed driver catalogs."
  ) else (
    call :log "KB3033929 detected."
  )
)

exit /b 0

:load_signing_policy
set "SIGNING_POLICY=testsigning"
set "MANIFEST_FILE=%SCRIPT_DIR%manifest.json"

set "FOUND_POLICY="
if exist "%MANIFEST_FILE%" (
  for /f "usebackq tokens=4 delims=^"" %%P in (`findstr /i "signing_policy" "%MANIFEST_FILE%"`) do (
    set "FOUND_POLICY=%%P"
    goto :load_signing_policy_parsed
  )
)

:load_signing_policy_parsed
if exist "%MANIFEST_FILE%" (
  if defined FOUND_POLICY (
    if /i "!FOUND_POLICY!"=="none" set "SIGNING_POLICY=none"
    if /i "!FOUND_POLICY!"=="testsigning" set "SIGNING_POLICY=testsigning"
    if /i "!FOUND_POLICY!"=="nointegritychecks" set "SIGNING_POLICY=nointegritychecks"
    if /i not "!FOUND_POLICY!"=="none" if /i not "!FOUND_POLICY!"=="testsigning" if /i not "!FOUND_POLICY!"=="nointegritychecks" (
      call :log "WARNING: manifest.json has unknown signing_policy: !FOUND_POLICY! (defaulting to testsigning)."
      set "SIGNING_POLICY=testsigning"
    ) else (
      call :log "Signing policy from manifest.json: %SIGNING_POLICY%"
    )
  ) else (
    call :log "WARNING: manifest.json present but signing_policy was not detected (defaulting to testsigning)."
  )
)

rem Explicit CLI overrides always win over the manifest/default.
if defined ARG_FORCE_SIGNING_POLICY (
  set "SIGNING_POLICY=%ARG_FORCE_SIGNING_POLICY%"
)
if "%ARG_FORCE_TESTSIGN%"=="1" set "SIGNING_POLICY=testsigning"
if "%ARG_FORCE_NOINTEGRITY%"=="1" set "SIGNING_POLICY=nointegritychecks"

call :log "Effective signing_policy: %SIGNING_POLICY%"
exit /b 0

:install_certs
set "CERT_DIR=%SCRIPT_DIR%certs"
call :log ""
call :log "Installing Aero certificate(s) from %CERT_DIR% ..."

if not exist "%CERT_DIR%" (
  call :log "WARNING: Certificate directory not found; skipping certificate installation."
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
  call :log "WARNING: No certificates found under %CERT_DIR% (expected *.cer/*.crt and/or *.p7b). Skipping certificate installation."
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

if /i "%SIGNING_POLICY%"=="none" (
  call :log "Signing policy is 'none'; not changing Test Signing / nointegritychecks."
  exit /b 0
)

if /i "%SIGNING_POLICY%"=="nointegritychecks" (
  call :log "Kernel driver signature enforcement is strict. This media is intended for drivers that may not be accepted by Win7 x64."
  call :log "To allow them to load, disable signature enforcement (nointegritychecks)."

  if "%NOINTEGRITY%"=="1" (
    call :log "nointegritychecks is already enabled."
    exit /b 0
  )

  set "DO_ENABLE=0"
  if "%ARG_FORCE_NOINTEGRITY%"=="1" (
    set "DO_ENABLE=1"
  ) else (
    choice /c YN /n /m "Enable nointegritychecks now? [Y/N] "
    if errorlevel 2 set "DO_ENABLE=0"
    if errorlevel 1 set "DO_ENABLE=1"
  )

  if "%DO_ENABLE%"=="0" (
    call :log "nointegritychecks was not enabled."
    exit /b 0
  )

  call :log "Enabling nointegritychecks via bcdedit (NOT RECOMMENDED)..."
  "%SYS32%\bcdedit.exe" /set nointegritychecks on >>"%LOG%" 2>&1
  if errorlevel 1 (
    call :log "ERROR: Failed to enable nointegritychecks."
    call :log "You may need to run this manually and reboot:"
    call :log "  bcdedit /set nointegritychecks on"
    if "%ARG_FORCE_NOINTEGRITY%"=="1" exit /b 1
    exit /b 0
  )

  > "%STATE_NOINTEGRITY%" echo nointegritychecks enabled by Aero Guest Tools on %DATE% %TIME%
  set "CHANGED_NOINTEGRITY=1"
  set "REBOOT_REQUIRED=1"
  call :log "nointegritychecks enabled. A reboot is required before it takes effect."
  exit /b 0
)

rem Default: signing_policy=testsigning (legacy behavior).
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
for /f "tokens=2 delims=:" %%A in ('findstr /i "Published name" "%OUT%"') do set "PUBLISHED=%%A"

rem pnputil on Windows 7 is not consistent about exit codes for idempotent "already imported" cases.
rem Treat common "already" messages as success so setup.cmd is safe to run multiple times.
if not "%RC%"=="0" (
  findstr /i /c:"already imported" /c:"already exists" /c:"already installed" /c:"already in the system" /c:"already in the driver store" "%OUT%" >nul 2>&1
  if not errorlevel 1 (
    call :log "pnputil reports the driver package is already present; continuing."
    set "RC=0"
  )
)

del /q "%OUT%" >nul 2>&1

if not "%RC%"=="0" (
  call :log "ERROR: pnputil -a failed for %INF% (exit code %RC%)."
  exit /b 1
)

if defined PUBLISHED (
  for /f "tokens=* delims= " %%B in ("!PUBLISHED!") do set "PUBLISHED=%%B"
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

set "TARGET_SVC=%AERO_VIRTIO_BLK_SERVICE%"
set "SCAN_LIST=%TEMP%\aerogt_infscan_%RANDOM%.txt"
del /q "%SCAN_LIST%" >nul 2>&1

set "INF_COUNT=0"
set "FOUND_MATCH=0"
set "MATCH_INF="

for /r "%DRIVER_DIR%" %%F in (*.inf) do (
  set /a INF_COUNT+=1
  >>"%SCAN_LIST%" echo %%~fF
  call :inf_contains_addservice "%%~fF" "%TARGET_SVC%"
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

call :log "ERROR: Configured AERO_VIRTIO_BLK_SERVICE=%TARGET_SVC% does not match any driver INF AddService name."
call :log "Expected to find an INF line (case-insensitive) like:"
call :log "  AddService = %TARGET_SVC%, ..."
call :log "  AddService = ^"%TARGET_SVC%^", ..."
call :log "Scanned INF files:"
for /f "usebackq delims=" %%I in ("%SCAN_LIST%") do call :log "  - %%I"
del /q "%SCAN_LIST%" >nul 2>&1
exit /b %EC_STORAGE_SERVICE_MISMATCH%

:inf_contains_addservice
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
    set "LEFT=!LEFT: =!"
    if /i "!LEFT!"=="AddService" (
      set "REST=!RIGHT!"
      for /f "tokens=* delims= " %%R in ("!REST!") do set "REST=%%R"
      set "REST=!REST:"=!"
      for /f "tokens=1 delims=, " %%S in ("!REST!") do set "SVC=%%S"
      if /i "!SVC!"=="!TARGET!" (
        endlocal & exit /b 0
      )
    )
  )
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
    exit /b 1
  )
)

rem Ensure the service exists and is BOOT_START.
set "SVC_KEY=HKLM\SYSTEM\CurrentControlSet\Services\%STOR_SERVICE%"
"%SYS32%\reg.exe" add "%SVC_KEY%" /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Type /t REG_DWORD /d 1 /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Start /t REG_DWORD /d 0 /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%SVC_KEY%" /v ErrorControl /t REG_DWORD /d 1 /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%SVC_KEY%" /v Group /t REG_SZ /d "SCSI miniport" /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%SVC_KEY%" /v ImagePath /t REG_EXPAND_SZ /d "system32\drivers\%STOR_SYS%" /f >>"%LOG%" 2>&1

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
"%SYS32%\reg.exe" add "%CDD_KEY%" /v Service /t REG_SZ /d "%SERVICE%" /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%CDD_KEY%" /v ClassGUID /t REG_SZ /d "%CLASSGUID%" /f >>"%LOG%" 2>&1
"%SYS32%\reg.exe" add "%CDD_KEY%" /v Class /t REG_SZ /d "SCSIAdapter" /f >>"%LOG%" 2>&1

if errorlevel 1 (
  call :log "ERROR: Failed to write CriticalDeviceDatabase key: %CDD_KEY%"
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
