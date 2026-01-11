@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem Aero Guest Tools uninstaller (best-effort).
rem WARNING: Uninstalling in-use storage drivers can make the VM unbootable.

rem Standard exit codes (stable for automation/scripted use).
set "EC_ADMIN_REQUIRED=10"

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
set "LOG=%INSTALL_ROOT%\uninstall.log"
set "PKG_LIST=%INSTALL_ROOT%\installed-driver-packages.txt"
set "CERT_LIST=%INSTALL_ROOT%\installed-certs.txt"
set "STATE_TESTSIGN=%INSTALL_ROOT%\testsigning.enabled-by-aero.txt"
set "STATE_NOINTEGRITY=%INSTALL_ROOT%\nointegritychecks.enabled-by-aero.txt"
set "STATE_STORAGE_SKIPPED=%INSTALL_ROOT%\storage-preseed.skipped.txt"

set "ARG_FORCE=0"
set "SIGNING_POLICY=testsigning"

set "ARG_NO_REBOOT=0"
if /i "%~1"=="/?" goto :usage
if /i "%~1"=="-h" goto :usage
if /i "%~1"=="--help" goto :usage
for %%A in (%*) do (
  if /i "%%~A"=="/force" set "ARG_FORCE=1"
  if /i "%%~A"=="/quiet" set "ARG_FORCE=1"
  if /i "%%~A"=="/quiet" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/noreboot" set "ARG_NO_REBOOT=1"
  if /i "%%~A"=="/no-reboot" set "ARG_NO_REBOOT=1"
)

call :require_admin_stdout
if errorlevel 1 (
  set "RC=%ERRORLEVEL%"
  popd >nul 2>&1
  exit /b %RC%
)

call :init_logging || goto :fail
call :log "Aero Guest Tools uninstall starting..."
call :log "Script dir: %SCRIPT_DIR%"
call :log "Logs: %LOG%"

call :require_admin || goto :fail
call :load_config || goto :fail
call :load_signing_policy

call :log ""
call :log "WARNING:"
call :log "  If this VM is currently booting from virtio-blk using the Aero storage driver,"
call :log "  removing that driver package or re-enabling signature enforcement can make the VM unbootable."
call :log ""
if exist "%STATE_STORAGE_SKIPPED%" (
  call :log "NOTE: setup.cmd previously ran with /skipstorage (marker exists: %STATE_STORAGE_SKIPPED%)."
  call :log "      This VM may still be configured to boot from AHCI, and virtio-blk boot-critical plumbing may be incomplete."
  call :log ""
)

if "%ARG_FORCE%"=="1" (
  call :log "Force mode: skipping confirmation prompt."
) else (
  choice /c YN /n /m "Continue with uninstall? [Y/N] "
  if errorlevel 2 (
    call :log "Uninstall cancelled."
    popd >nul 2>&1
    exit /b 0
  )
)

call :remove_driver_packages || goto :fail
call :remove_certs || goto :fail
call :maybe_disable_testsigning || goto :fail
call :maybe_disable_nointegritychecks || goto :fail

call :log ""
call :log "Uninstall complete."
call :maybe_reboot
popd >nul 2>&1
exit /b 0

:usage
echo Usage: uninstall.cmd [options]
echo.
echo Options:
echo   /force, /quiet       Non-interactive: skip confirmation and leave signature mode unchanged
echo                        (/quiet also implies /noreboot)
echo   /noreboot            Do not prompt to reboot/shutdown at the end
echo.
echo Logs are written to C:\AeroGuestTools\uninstall.log
popd >nul 2>&1
exit /b 0

:fail
set "RC=%ERRORLEVEL%"
call :log ""
call :log "ERROR: uninstall failed (exit code %RC%). See %LOG% for details."
popd >nul 2>&1
exit /b %RC%

:init_logging
if not exist "%INSTALL_ROOT%" mkdir "%INSTALL_ROOT%" >nul 2>&1
>>"%LOG%" echo ============================================================
>>"%LOG%" echo [%DATE% %TIME%] Aero Guest Tools uninstall starting
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
  echo Right-click uninstall.cmd and choose 'Run as administrator'.
  exit /b %EC_ADMIN_REQUIRED%
)
exit /b 0

:require_admin
call :log "Checking for Administrator privileges..."
"%SYS32%\fsutil.exe" dirty query %SYSTEMDRIVE% >nul 2>&1
if errorlevel 1 (
  call :log "ERROR: Administrator privileges are required."
  call :log "Right-click uninstall.cmd and choose 'Run as administrator'."
  exit /b %EC_ADMIN_REQUIRED%
)
exit /b 0

:load_config
set "CONFIG_FILE=%SCRIPT_DIR%config\devices.cmd"
if not exist "%CONFIG_FILE%" (
  call :log "ERROR: Missing config file: %CONFIG_FILE%"
  exit /b 1
)
call "%CONFIG_FILE%"
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
      call :log "Signing policy from manifest.json: !SIGNING_POLICY!"
    )
  )
)

call :log "Effective signing_policy: %SIGNING_POLICY%"
exit /b 0

:remove_driver_packages
call :log ""
call :log "Removing Aero driver packages from Driver Store (best-effort)..."

if not exist "%PKG_LIST%" (
  call :log "No recorded package list found (%PKG_LIST%). Skipping driver package removal."
  exit /b 0
)

for /f "usebackq delims=" %%P in ("%PKG_LIST%") do (
  if not "%%~P"=="" (
    call :log "  - pnputil -d %%~P"
    "%SYS32%\pnputil.exe" -d "%%~P" >>"%LOG%" 2>&1
  )
)

exit /b 0

:remove_certs
call :log ""
call :log "Removing Aero certificates (best-effort)..."

if not exist "%CERT_LIST%" (
  call :log "No recorded certificate thumbprints found (%CERT_LIST%). Skipping certificate removal."
  exit /b 0
)

for /f "usebackq delims=" %%H in ("%CERT_LIST%") do (
  if not "%%~H"=="" (
    call :log "  - Root: %%~H"
    "%SYS32%\certutil.exe" -delstore Root "%%~H" >>"%LOG%" 2>&1
    call :log "  - TrustedPublisher: %%~H"
    "%SYS32%\certutil.exe" -delstore TrustedPublisher "%%~H" >>"%LOG%" 2>&1
  )
)

exit /b 0

:maybe_disable_testsigning
rem Only prompt if we previously enabled it.
if not exist "%STATE_TESTSIGN%" exit /b 0

if "%ARG_FORCE%"=="1" (
  call :log ""
  call :log "Force mode: leaving Test Signing unchanged."
  exit /b 0
)

call :log ""
call :log "Test Signing may have been enabled by Aero Guest Tools."
choice /c YN /n /m "Disable Test Signing now? (bcdedit /set testsigning off) [Y/N] "
if errorlevel 2 exit /b 0

call :log "Disabling Test Signing..."
"%SYS32%\bcdedit.exe" /set testsigning off >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "WARNING: Failed to disable Test Signing."
  exit /b 0
)

del /q "%STATE_TESTSIGN%" >nul 2>&1
exit /b 0

:maybe_disable_nointegritychecks
rem Only prompt if we previously enabled it.
if not exist "%STATE_NOINTEGRITY%" exit /b 0

if "%ARG_FORCE%"=="1" (
  call :log ""
  call :log "Force mode: leaving nointegritychecks unchanged."
  exit /b 0
)

call :log ""
call :log "nointegritychecks may have been enabled by Aero Guest Tools."
choice /c YN /n /m "Disable nointegritychecks now? (bcdedit /set nointegritychecks off) [Y/N] "
if errorlevel 2 exit /b 0

call :log "Disabling nointegritychecks..."
"%SYS32%\bcdedit.exe" /set nointegritychecks off >>"%LOG%" 2>&1
if errorlevel 1 (
  call :log "WARNING: Failed to disable nointegritychecks."
  exit /b 0
)

del /q "%STATE_NOINTEGRITY%" >nul 2>&1
exit /b 0

:maybe_reboot
if "%ARG_NO_REBOOT%"=="1" exit /b 0

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

