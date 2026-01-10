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

REM Set to 1 to copy the located payload to C:\Aero (recommended).
REM This makes the next-boot driver install independent of removable/config media.
set "AERO_COPY_PAYLOAD_TO_C=1"

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
  echo Expected "C:\Aero\" or a payload root containing "Drivers\" and "Scripts\" (and optionally "Cert\"/"Certs\").
  echo See scripts/README.md for payload discovery behaviour (drive scanning, marker files, and supported layouts).
  exit /b 10
)

echo Using Aero payload root: "%AERO_ROOT%"

if /i "%AERO_COPY_PAYLOAD_TO_C%"=="1" (
  if /i not "%AERO_ROOT%"=="C:\Aero" (
    echo Copying payload to "C:\Aero" for persistence across reboot...
    set "AERO_ORIG_ROOT=%AERO_ROOT%"
    call :COPY_PAYLOAD_TO_C "%AERO_ROOT%" "C:\Aero"
    if errorlevel 1 (
      echo WARNING: Failed to copy payload to "C:\Aero". Continuing with "%AERO_ORIG_ROOT%".
      set "AERO_ROOT=%AERO_ORIG_ROOT%"
    ) else (
      set "AERO_ROOT=C:\Aero"
      echo Payload copy complete. Using local payload root: "%AERO_ROOT%"
    )
  ) else (
    echo Payload already located at "C:\Aero"; copy not needed.
  )
) else (
  echo Payload copy disabled (AERO_COPY_PAYLOAD_TO_C=%AERO_COPY_PAYLOAD_TO_C%).
)

REM Certificate file is optional. Accept multiple common names so the unattended
REM workflow doesn't depend on a single artifact naming convention.
set "AERO_CERT_FILE=%AERO_ROOT%\Cert\aero_test.cer"
if not exist "%AERO_CERT_FILE%" set "AERO_CERT_FILE=%AERO_ROOT%\Cert\aero-test.cer"
if not exist "%AERO_CERT_FILE%" set "AERO_CERT_FILE=%AERO_ROOT%\Cert\aero-test-root.cer"
if not exist "%AERO_CERT_FILE%" set "AERO_CERT_FILE=%AERO_ROOT%\Certs\AeroTestRoot.cer"

if exist "%AERO_CERT_FILE%" (
  echo Importing test certificate: "%AERO_CERT_FILE%"
  certutil -addstore -f Root "%AERO_CERT_FILE%"
  echo certutil Root exit code: !errorlevel!
  certutil -addstore -f TrustedPublisher "%AERO_CERT_FILE%"
  echo certutil TrustedPublisher exit code: !errorlevel!
) else (
  echo No certificate found under "%AERO_ROOT%\Cert\" or "%AERO_ROOT%\Certs\". Skipping certificate import.
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

REM Prefer the script from the local staged payload if present.
set "AERO_INSTALL_SCRIPT=C:\Aero\Scripts\InstallDriversOnce.cmd"
if not exist "%AERO_INSTALL_SCRIPT%" (
  set "AERO_INSTALL_SCRIPT=%AERO_ROOT%\Scripts\InstallDriversOnce.cmd"
)
if not exist "%AERO_INSTALL_SCRIPT%" (
  echo ERROR: InstallDriversOnce.cmd not found.
  echo Checked: "C:\Aero\Scripts\InstallDriversOnce.cmd"
  echo Checked: "%AERO_ROOT%\Scripts\InstallDriversOnce.cmd"
  exit /b 30
)

echo Creating scheduled task "%AERO_TASK_NAME%" to install Aero drivers on next boot...
REM The task action we want is:
REM   cmd.exe /c ""C:\...\InstallDriversOnce.cmd""
echo Task action: cmd.exe /c ""%AERO_INSTALL_SCRIPT%""
REM Note: we need the nested quotes around the script path so cmd.exe /c
REM preserves quoting when executing a path containing spaces.
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

:COPY_PAYLOAD_TO_C
set "AERO_SRC_ROOT=%~1"
set "AERO_DST_ROOT=%~2"
if "%AERO_SRC_ROOT%"=="" exit /b 1
if "%AERO_DST_ROOT%"=="" exit /b 1

mkdir "%AERO_DST_ROOT%" >nul 2>&1

set "AERO_COPY_FAILED=0"

set "AERO_OS_ARCH=x86"
if exist "%WINDIR%\SysWOW64\" set "AERO_OS_ARCH=amd64"

if exist "%AERO_SRC_ROOT%\Drivers\" (
  REM If the source uses the config-media driver layout (WinPE/Offline + arch),
  REM copy only the matching architecture folders. Otherwise copy the full Drivers tree.
  set "AERO_DRIVER_LAYOUT=flat"
  if exist "%AERO_SRC_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%\" set "AERO_DRIVER_LAYOUT=arch"
  if exist "%AERO_SRC_ROOT%\Drivers\Offline\%AERO_OS_ARCH%\" set "AERO_DRIVER_LAYOUT=arch"

  if /i "%AERO_DRIVER_LAYOUT%"=="arch" (
    echo [COPY] Drivers (arch=%AERO_OS_ARCH%) -> "%AERO_DST_ROOT%\Drivers\"

    if exist "%AERO_SRC_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%\" (
      echo [COPY] Drivers\WinPE\%AERO_OS_ARCH%\ -> "%AERO_DST_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%\"
      xcopy "%AERO_SRC_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%" "%AERO_DST_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%\" /E /I /H /R /Y /C /Q
      set "RC=!errorlevel!"
      echo [COPY] xcopy Drivers\WinPE exit code: !RC!
      if !RC! GEQ 4 set "AERO_COPY_FAILED=1"
    ) else (
      echo [COPY] NOTE: "%AERO_SRC_ROOT%\Drivers\WinPE\%AERO_OS_ARCH%\" not found; skipping.
    )

    if exist "%AERO_SRC_ROOT%\Drivers\Offline\%AERO_OS_ARCH%\" (
      echo [COPY] Drivers\Offline\%AERO_OS_ARCH%\ -> "%AERO_DST_ROOT%\Drivers\Offline\%AERO_OS_ARCH%\"
      xcopy "%AERO_SRC_ROOT%\Drivers\Offline\%AERO_OS_ARCH%" "%AERO_DST_ROOT%\Drivers\Offline\%AERO_OS_ARCH%\" /E /I /H /R /Y /C /Q
      set "RC=!errorlevel!"
      echo [COPY] xcopy Drivers\Offline exit code: !RC!
      if !RC! GEQ 4 set "AERO_COPY_FAILED=1"
    ) else (
      echo [COPY] NOTE: "%AERO_SRC_ROOT%\Drivers\Offline\%AERO_OS_ARCH%\" not found; skipping.
    )
  ) else (
    echo [COPY] Drivers\ -> "%AERO_DST_ROOT%\Drivers\"
    xcopy "%AERO_SRC_ROOT%\Drivers" "%AERO_DST_ROOT%\Drivers\" /E /I /H /R /Y /C /Q
    set "RC=!errorlevel!"
    echo [COPY] xcopy Drivers exit code: !RC!
    if !RC! GEQ 4 set "AERO_COPY_FAILED=1"
  )
) else (
  echo [COPY] WARNING: "%AERO_SRC_ROOT%\Drivers\" not found.
  set "AERO_COPY_FAILED=1"
)

if exist "%AERO_SRC_ROOT%\Scripts\" (
  echo [COPY] Scripts\ -> "%AERO_DST_ROOT%\Scripts\"
  xcopy "%AERO_SRC_ROOT%\Scripts" "%AERO_DST_ROOT%\Scripts\" /E /I /H /R /Y /C /Q
  set "RC=!errorlevel!"
  echo [COPY] xcopy Scripts exit code: !RC!
  if !RC! GEQ 4 set "AERO_COPY_FAILED=1"
) else (
  echo [COPY] WARNING: "%AERO_SRC_ROOT%\Scripts\" not found.
  set "AERO_COPY_FAILED=1"
)

if exist "%AERO_SRC_ROOT%\Cert\" (
  echo [COPY] Cert\ -> "%AERO_DST_ROOT%\Cert\"
  xcopy "%AERO_SRC_ROOT%\Cert" "%AERO_DST_ROOT%\Cert\" /E /I /H /R /Y /C /Q
  echo [COPY] xcopy Cert exit code: !errorlevel!
)

if exist "%AERO_SRC_ROOT%\Certs\" (
  echo [COPY] Certs\ -> "%AERO_DST_ROOT%\Certs\"
  xcopy "%AERO_SRC_ROOT%\Certs" "%AERO_DST_ROOT%\Certs\" /E /I /H /R /Y /C /Q
  echo [COPY] xcopy Certs exit code: !errorlevel!
)

if not exist "%AERO_DST_ROOT%\Drivers\" (
  echo [COPY] ERROR: Destination missing required folder: "%AERO_DST_ROOT%\Drivers\"
  exit /b 2
)
if not exist "%AERO_DST_ROOT%\Scripts\" (
  echo [COPY] ERROR: Destination missing required folder: "%AERO_DST_ROOT%\Scripts\"
  exit /b 2
)

if "%AERO_COPY_FAILED%"=="1" (
  echo [COPY] ERROR: One or more required payload folders failed to copy.
  exit /b 3
)

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

REM 3) Scan drive letters for common payload layouts and marker files.
REM    Marker files: AERO.TAG (preferred) or AERO_CONFIG.MEDIA (also accepted).
call :SCAN_FOR_AERO_MARKER
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

:SCAN_FOR_AERO_MARKER
for %%D in (C D E F G H I J K L M N O P Q R S T U V W X Y Z) do (
  if exist "%%D:\nul" (
    REM Common removable/config media layout (no marker file required):
    REM   X:\Aero\Drivers\
    REM   X:\Aero\Scripts\
    if exist "%%D:\Aero\Drivers\" (
      if exist "%%D:\Aero\Scripts\" (
        call :TRY_AERO_ROOT "%%D:\Aero"
        if defined AERO_ROOT exit /b 0
      )
    )
    REM Common config media layout at drive root (no marker file required):
    REM   X:\Drivers\
    REM   X:\Scripts\
    if exist "%%D:\Drivers\" (
      if exist "%%D:\Scripts\InstallDriversOnce.cmd" (
        call :TRY_AERO_ROOT "%%D:\"
        if defined AERO_ROOT exit /b 0
      )
    )
    if exist "%%D:\Aero\AERO.TAG" (
      call :TRY_AERO_ROOT "%%D:\Aero"
      if defined AERO_ROOT exit /b 0
    )
    if exist "%%D:\Aero\AERO_CONFIG.MEDIA" (
      call :TRY_AERO_ROOT "%%D:\Aero"
      if defined AERO_ROOT exit /b 0
    )
    if exist "%%D:\AERO.TAG" (
      call :TRY_AERO_ROOT "%%D:\"
      if defined AERO_ROOT exit /b 0
    )
    if exist "%%D:\AERO_CONFIG.MEDIA" (
      call :TRY_AERO_ROOT "%%D:\"
      if defined AERO_ROOT exit /b 0
    )
  )
)
exit /b 0
