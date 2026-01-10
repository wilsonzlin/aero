@echo off
setlocal EnableExtensions EnableDelayedExpansion

REM ================================================================
REM Aero Win7 SP1 post-install automation (SetupComplete)
REM
REM This script is intended to be copied to:
REM   %WINDIR%\Setup\Scripts\SetupComplete.cmd
REM by an unattend "specialize" RunSynchronous command.
REM
REM It runs as SYSTEM near the end of Windows setup.
REM ================================================================

REM Set to 1 to also disable integrity checks (NOT recommended unless required).
REM   - testsigning is required for test-signed drivers
REM   - nointegritychecks is a stronger setting and reduces security
set "AERO_ENABLE_NOINTEGRITYCHECKS=0"

set "AERO_LOG_FILE=%WINDIR%\Temp\aero-setup.log"
if not exist "%WINDIR%\Temp" mkdir "%WINDIR%\Temp" >nul 2>&1

call :MAIN >> "%AERO_LOG_FILE%" 2>&1
exit /b %errorlevel%

:MAIN
echo ================================================================
echo [%DATE% %TIME%] Aero SetupComplete starting

set "AERO_MARKER_FILE=%WINDIR%\Temp\aero-setupcomplete.done"
if exist "%AERO_MARKER_FILE%" (
  echo Marker already present: "%AERO_MARKER_FILE%"
  echo Exiting without changes.
  exit /b 0
)

set "AERO_ROOT="
call :FIND_AERO_ROOT

if not defined AERO_ROOT (
  echo ERROR: Could not locate Aero payload directory.
  echo Expected "C:\Aero\" or a payload root containing "Drivers\", "Cert\", and "Scripts\".
  echo See README.md for AERO.TAG scanning behaviour.
  exit /b 10
)

echo Using Aero payload root: "%AERO_ROOT%"

set "AERO_CERT_FILE=%AERO_ROOT%\Cert\aero_test.cer"
if not exist "%AERO_CERT_FILE%" set "AERO_CERT_FILE=%AERO_ROOT%\Certs\AeroTestRoot.cer"
if exist "%AERO_CERT_FILE%" (
  echo Importing test certificate: "%AERO_CERT_FILE%"
  certutil -addstore -f Root "%AERO_CERT_FILE%"
  echo certutil Root exit code: !errorlevel!
  certutil -addstore -f TrustedPublisher "%AERO_CERT_FILE%"
  echo certutil TrustedPublisher exit code: !errorlevel!
) else (
  echo No certificate found at "%AERO_ROOT%\Cert\aero_test.cer" (or "%AERO_ROOT%\Certs\AeroTestRoot.cer"). Skipping certificate import.
)

echo Enabling test signing...
bcdedit /set testsigning on
if errorlevel 1 (
  echo ERROR: Failed to enable testsigning via bcdedit.
  exit /b 20
)

if /i "%AERO_ENABLE_NOINTEGRITYCHECKS%"=="1" (
  echo Enabling nointegritychecks (AERO_ENABLE_NOINTEGRITYCHECKS=1)...
  bcdedit /set nointegritychecks on
  if errorlevel 1 (
    echo ERROR: Failed to enable nointegritychecks via bcdedit.
    exit /b 21
  )
) else (
  echo nointegritychecks not enabled (AERO_ENABLE_NOINTEGRITYCHECKS=%AERO_ENABLE_NOINTEGRITYCHECKS%).
)

set "AERO_TASK_NAME=Aero-InstallDriversOnce"
set "AERO_INSTALL_SCRIPT=%AERO_ROOT%\Scripts\InstallDriversOnce.cmd"
if not exist "%AERO_INSTALL_SCRIPT%" (
  echo ERROR: InstallDriversOnce.cmd not found: "%AERO_INSTALL_SCRIPT%"
  exit /b 30
)

echo Creating scheduled task "%AERO_TASK_NAME%" to install Aero drivers on next boot...
REM Note: quoting here is intentionally gnarly; the resulting task action is:
REM   cmd.exe /c ""C:\...\InstallDriversOnce.cmd""
schtasks /Create /F /SC ONSTART /TN "%AERO_TASK_NAME%" /RU SYSTEM /RL HIGHEST /TR "cmd.exe /c """"%AERO_INSTALL_SCRIPT%"""""
if errorlevel 1 (
  echo ERROR: Failed to create scheduled task "%AERO_TASK_NAME%".
  exit /b 31
)

schtasks /Query /TN "%AERO_TASK_NAME%" >nul 2>&1
if errorlevel 1 (
  echo ERROR: Scheduled task "%AERO_TASK_NAME%" not found after creation.
  exit /b 32
)

echo Creating marker file: "%AERO_MARKER_FILE%"
> "%AERO_MARKER_FILE%" echo done
if errorlevel 1 (
  echo ERROR: Failed to create marker file "%AERO_MARKER_FILE%".
  exit /b 33
)

echo Rebooting now to apply boot configuration changes...
shutdown /r /t 0
exit /b 0

:FIND_AERO_ROOT
REM 1) Primary expected location
call :TRY_AERO_ROOT "C:\Aero"
if defined AERO_ROOT exit /b 0

REM 2) Unattend config set root (if available)
if defined configsetroot (
  call :TRY_AERO_ROOT "%configsetroot%"
  if defined AERO_ROOT exit /b 0
  call :TRY_AERO_ROOT "%configsetroot%\Aero"
  if defined AERO_ROOT exit /b 0
)

REM 3) Scan drive letters for AERO.TAG markers
call :SCAN_FOR_AERO_TAG
exit /b 0

:TRY_AERO_ROOT
set "AERO_CANDIDATE=%~1"
if "%AERO_CANDIDATE%"=="" exit /b 1
if exist "%AERO_CANDIDATE%\Drivers\" (
  if exist "%AERO_CANDIDATE%\Scripts\" (
    set "AERO_ROOT=%AERO_CANDIDATE%"
    exit /b 0
  )
)
exit /b 1

:SCAN_FOR_AERO_TAG
for %%D in (C D E F G H I J K L M N O P Q R S T U V W X Y Z) do (
  if exist "%%D:\nul" (
    if exist "%%D:\Aero\AERO.TAG" (
      call :TRY_AERO_ROOT "%%D:\Aero"
      if defined AERO_ROOT exit /b 0
    )
    if exist "%%D:\AERO.TAG" (
      call :TRY_AERO_ROOT "%%D:\"
      if defined AERO_ROOT exit /b 0
    )
  )
)
exit /b 0
