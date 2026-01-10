@echo off
setlocal EnableExtensions EnableDelayedExpansion

REM ================================================================
REM Aero Win7 SP1 post-install automation (InstallDriversOnce)
REM
REM Intended to run once as SYSTEM at boot via a scheduled task created
REM by SetupComplete.cmd.
REM ================================================================

REM Set to 1 to reboot if at least one driver install command succeeded.
set "AERO_REBOOT_AFTER_DRIVER_INSTALL=0"

set "AERO_LOG_FILE=%WINDIR%\Temp\aero-driver-install.log"
if not exist "%WINDIR%\Temp" mkdir "%WINDIR%\Temp" >nul 2>&1

call :MAIN >> "%AERO_LOG_FILE%" 2>&1
exit /b %errorlevel%

:MAIN
echo ================================================================
echo [%DATE% %TIME%] Aero InstallDriversOnce starting

set "AERO_MARKER_FILE=%WINDIR%\Temp\aero-install-drivers.done"
set "AERO_TASK_NAME=Aero-InstallDriversOnce"

if exist "%AERO_MARKER_FILE%" (
  echo Marker already present: "%AERO_MARKER_FILE%"
  echo Skipping driver installation.
  call :DELETE_TASK
  exit /b 0
)

set "AERO_PNPUTIL="
call :FIND_PNPUTIL
echo Using pnputil: "%AERO_PNPUTIL%"
"%AERO_PNPUTIL%" /? >nul 2>&1
if errorlevel 1 (
  echo ERROR: pnputil is not available; cannot install driver packages.
  call :DELETE_TASK
  exit /b 12
)

set "AERO_ROOT="
call :FIND_AERO_ROOT
if not defined AERO_ROOT (
  echo ERROR: Could not locate Aero payload directory.
  echo Expected "C:\Aero\" or a payload root containing "Drivers\", "Cert\", and "Scripts\".
  call :DELETE_TASK
  exit /b 10
)

set "AERO_DRIVERS_DIR=%AERO_ROOT%\Drivers"
if not exist "%AERO_DRIVERS_DIR%\" (
  echo ERROR: Drivers directory not found: "%AERO_DRIVERS_DIR%"
  call :DELETE_TASK
  exit /b 11
)

echo Using Aero payload root: "%AERO_ROOT%"
echo Drivers directory: "%AERO_DRIVERS_DIR%"

set "AERO_FAIL_LIST=%WINDIR%\Temp\aero-driver-install.failures.txt"
if exist "%AERO_FAIL_LIST%" del /f /q "%AERO_FAIL_LIST%" >nul 2>&1

set "AERO_FOUND_INF=0"
set "AERO_ANY_SUCCESS=0"
set "AERO_EXITCODE=0"

for /r "%AERO_DRIVERS_DIR%" %%F in (*.inf) do (
  set "AERO_FOUND_INF=1"
  echo ------------------------------------------------
  echo Installing driver package: "%%F"
  "%AERO_PNPUTIL%" -i -a "%%F"
  set "RC=!errorlevel!"
  if not "!RC!"=="0" (
    echo ERROR: pnputil failed with exit code !RC! for "%%F"
    >> "%AERO_FAIL_LIST%" echo "%%F"
    set "AERO_EXITCODE=1"
  ) else (
    set "AERO_ANY_SUCCESS=1"
  )
)

if "%AERO_FOUND_INF%"=="0" (
  echo WARNING: No .inf files found under "%AERO_DRIVERS_DIR%".
)

if exist "%AERO_FAIL_LIST%" (
  echo ------------------------------------------------
  echo One or more driver packages failed to install. Failed INF paths:
  type "%AERO_FAIL_LIST%"
)

echo Creating marker file: "%AERO_MARKER_FILE%"
> "%AERO_MARKER_FILE%" echo done
if errorlevel 1 (
  echo ERROR: Failed to create marker file "%AERO_MARKER_FILE%".
  set "AERO_EXITCODE=12"
)

call :DELETE_TASK

if /i "%AERO_REBOOT_AFTER_DRIVER_INSTALL%"=="1" (
  if "%AERO_ANY_SUCCESS%"=="1" (
    echo Rebooting to complete driver installation...
    shutdown /r /t 0
  ) else (
    echo No successful driver installs detected; skipping reboot.
  )
)

exit /b %AERO_EXITCODE%

:DELETE_TASK
schtasks /Query /TN "%AERO_TASK_NAME%" >nul 2>&1
if errorlevel 1 (
  echo Scheduled task "%AERO_TASK_NAME%" not present.
  exit /b 0
)
echo Deleting scheduled task "%AERO_TASK_NAME%"...
schtasks /Delete /TN "%AERO_TASK_NAME%" /F
exit /b 0

:FIND_PNPUTIL
set "AERO_PNPUTIL=%WINDIR%\Sysnative\pnputil.exe"
if exist "%AERO_PNPUTIL%" exit /b 0
set "AERO_PNPUTIL=%WINDIR%\System32\pnputil.exe"
if exist "%AERO_PNPUTIL%" exit /b 0
set "AERO_PNPUTIL=%WINDIR%\SysWOW64\pnputil.exe"
if exist "%AERO_PNPUTIL%" exit /b 0
set "AERO_PNPUTIL=pnputil.exe"
exit /b 0

:FIND_AERO_ROOT
REM 0) If this script is running from within a payload's "Scripts\" directory,
REM    use the parent directory as the payload root.
call :TRY_AERO_ROOT_FROM_SCRIPT_DIR
if defined AERO_ROOT exit /b 0

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

:TRY_AERO_ROOT_FROM_SCRIPT_DIR
set "AERO_SCRIPT_DIR=%~dp0"
if "%AERO_SCRIPT_DIR%"=="" exit /b 1
for %%P in ("%AERO_SCRIPT_DIR%..") do set "AERO_CANDIDATE=%%~fP"
call :TRY_AERO_ROOT "%AERO_CANDIDATE%"
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
