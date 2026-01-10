@echo off
setlocal enableextensions enabledelayedexpansion

rem Test-signing helper for the AeroGPU Windows 7 driver package.
rem
rem What it does:
rem   1) Enables Windows test-signing mode (bcdedit)
rem   2) Creates a self-signed test certificate (Code Signing EKU)
rem   3) Installs that cert into:
rem        - LocalMachine\Root            (Trusted Root Certification Authorities)
rem        - LocalMachine\TrustedPublisher (Trusted Publishers)
rem   4) (Optionally) runs Inf2Cat to generate a .cat
rem   5) Signs the .sys, .dll, and .cat files with signtool
rem
rem Requirements:
rem   - Run from an elevated command prompt (Administrator).
rem   - Windows SDK/WDK tooling on PATH:
rem       makecert.exe, signtool.exe
rem       inf2cat.exe (recommended; required if your INF has CatalogFile=...)
rem
rem Usage:
rem   sign_test.cmd
rem   sign_test.cmd --no-bcdedit
rem   sign_test.cmd --no-inf2cat

set "SCRIPT_DIR=%~dp0"
rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"
set "BCDEDIT=%SYS32%\bcdedit.exe"
set "CERTUTIL=%SYS32%\certutil.exe"
pushd "%SCRIPT_DIR%" >nul

set "NO_BCDEDIT=0"
set "NO_INF2CAT=0"

if /i "%~1"=="--no-bcdedit" set "NO_BCDEDIT=1"
if /i "%~1"=="--no-inf2cat" set "NO_INF2CAT=1"

rem Admin check (net session requires elevation)
net session >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] sign_test.cmd must be run as Administrator.
  popd >nul
  exit /b 1
)

if "%NO_BCDEDIT%"=="0" (
  echo [INFO] Enabling test-signing mode...
  "%BCDEDIT%" /set testsigning on >nul 2>&1
  if not "%ERRORLEVEL%"=="0" (
    echo [WARN] bcdedit failed. If you are on a host with Secure Boot enabled, test-signing may be blocked.
    echo [WARN] On a Win7 VM, bcdedit should succeed.
  ) else (
    echo [OK] Test-signing mode enabled. Reboot is required before Windows enforces this setting.
  )
)

set "CERT_BASE=aerogpu_test"
set "CERT_NAME=AeroGPU Test Signing"
set "CERT_SUBJECT=CN=%CERT_NAME%"
set "CERT_PASSWORD=aerogpu"

set "CER_FILE=%CERT_BASE%.cer"
set "PFX_FILE=%CERT_BASE%.pfx"

call :RequireTool makecert.exe
if not "%ERRORLEVEL%"=="0" goto :Fail
call :RequireTool signtool.exe
if not "%ERRORLEVEL%"=="0" goto :Fail

set "NEED_CERT=0"
if not exist "%CER_FILE%" set "NEED_CERT=1"
if not exist "%PFX_FILE%" set "NEED_CERT=1"

if "%NEED_CERT%"=="1" (
  echo [INFO] Creating test certificate "%CERT_NAME%"...
  if exist "%CER_FILE%" del "%CER_FILE%" >nul 2>&1
  if exist "%PFX_FILE%" del "%PFX_FILE%" >nul 2>&1

  rem Create a self-signed code-signing cert in LocalMachine\My and write it to a .cer file.
  makecert.exe -r -pe -ss My -sr LocalMachine -n "%CERT_SUBJECT%" -eku 1.3.6.1.5.5.7.3.3 "%CER_FILE%"
  if not "%ERRORLEVEL%"=="0" (
    echo [ERROR] makecert.exe failed.
    goto :Fail
  )

  rem Export the private key to a .pfx for signtool /f signing.
  "%CERTUTIL%" -f -p "%CERT_PASSWORD%" -exportPFX My "%CERT_NAME%" "%PFX_FILE%" >nul
  if not "%ERRORLEVEL%"=="0" (
    echo [ERROR] certutil -exportPFX failed.
    goto :Fail
  )
)

echo [INFO] Importing certificate into Trusted Root and Trusted Publishers...
"%CERTUTIL%" -f -addstore "Root" "%CER_FILE%" >nul
"%CERTUTIL%" -f -addstore "TrustedPublisher" "%CER_FILE%" >nul

if "%NO_INF2CAT%"=="0" (
  rem If INF declares CatalogFile=..., pnputil expects the .cat to exist.
  call :MaybeInf2Cat
  if not "%ERRORLEVEL%"=="0" goto :Fail
)

echo [INFO] Signing package files...

for %%F in (*.sys *.dll *.cat) do (
  echo [INFO]   signing %%F
  signtool.exe sign /v /fd sha1 /f "%PFX_FILE%" /p "%CERT_PASSWORD%" "%%F"
  if not "!ERRORLEVEL!"=="0" (
    echo [ERROR] signtool failed for %%F
    popd >nul
    exit /b 1
  )
)

echo [OK] Package signed.
echo [NOTE] If you changed test-signing mode, reboot Windows before installing the driver.
popd >nul
exit /b 0

rem -----------------------------------------------------------------
:Fail
popd >nul
exit /b 1

rem -----------------------------------------------------------------
:RequireTool
where "%~1" >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] Required tool not found on PATH: %~1
  echo [ERROR] Install the Windows 7 WDK (7600) or a Windows SDK/WDK that includes it, then retry.
  exit /b 1
)
exit /b 0

rem -----------------------------------------------------------------
:MaybeInf2Cat
where inf2cat.exe >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [WARN] inf2cat.exe not found; skipping catalog generation.
  echo [WARN] If your INF has CatalogFile=..., installation via pnputil may fail until a .cat is present.
  exit /b 0
)

set "INF2CAT_TMPBASE=%TEMP%\aerogpu_inf2cat_%RANDOM%"
mkdir "%INF2CAT_TMPBASE%" >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] Failed to create temp directory: "%INF2CAT_TMPBASE%"
  exit /b 1
)

rem Detect OS architecture (affects which files are required and which /os target to use).
set "INF2CAT_ARCH=x86"
if /i "%PROCESSOR_ARCHITECTURE%"=="AMD64" set "INF2CAT_ARCH=amd64"
if /i "%PROCESSOR_ARCHITEW6432%"=="AMD64" set "INF2CAT_ARCH=amd64"

set "INF2CAT_OS=7_X86"
if /i "%INF2CAT_ARCH%"=="amd64" set "INF2CAT_OS=7_X64"

echo [INFO] Inf2Cat target: %INF2CAT_OS% (arch=%INF2CAT_ARCH%)

rem Always generate the base catalog if the base INF is present.
if exist "aerogpu.inf" (
  if /i "%INF2CAT_ARCH%"=="amd64" (
    call :Inf2CatOne "aerogpu.inf" "aerogpu.cat" aerogpu.sys aerogpu_d3d9.dll aerogpu_d3d9_x64.dll
  ) else (
    call :Inf2CatOne "aerogpu.inf" "aerogpu.cat" aerogpu.sys aerogpu_d3d9.dll
  )
  if not "%ERRORLEVEL%"=="0" goto :Inf2CatFail
)

rem Only generate the optional DX11 catalog if its files are present.
if exist "aerogpu_dx11.inf" (
  if /i "%INF2CAT_ARCH%"=="amd64" (
    if exist "aerogpu_d3d10.dll" if exist "aerogpu_d3d10_x64.dll" (
      call :Inf2CatOne "aerogpu_dx11.inf" "aerogpu_dx11.cat" aerogpu.sys aerogpu_d3d9.dll aerogpu_d3d9_x64.dll aerogpu_d3d10.dll aerogpu_d3d10_x64.dll
      if not "%ERRORLEVEL%"=="0" goto :Inf2CatFail
    ) else (
      echo [INFO] Skipping aerogpu_dx11.inf catalog generation (optional D3D10/11 UMDs not found).
    )
  ) else (
    if exist "aerogpu_d3d10.dll" (
      call :Inf2CatOne "aerogpu_dx11.inf" "aerogpu_dx11.cat" aerogpu.sys aerogpu_d3d9.dll aerogpu_d3d10.dll
      if not "%ERRORLEVEL%"=="0" goto :Inf2CatFail
    ) else (
      echo [INFO] Skipping aerogpu_dx11.inf catalog generation (optional D3D10/11 UMDs not found).
    )
  )
)

rd /s /q "%INF2CAT_TMPBASE%" >nul 2>&1
set "INF2CAT_TMPBASE="
exit /b 0

:Inf2CatFail
rd /s /q "%INF2CAT_TMPBASE%" >nul 2>&1
set "INF2CAT_TMPBASE="
exit /b 1

rem -----------------------------------------------------------------
:Inf2CatOne
set "INF2CAT_INF=%~1"
set "INF2CAT_CAT=%~2"
set "INF2CAT_INFBASENAME=%~n1"
shift
shift

set "INF2CAT_WORK=%INF2CAT_TMPBASE%\%INF2CAT_INFBASENAME%"
mkdir "%INF2CAT_WORK%" >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] Failed to create: "%INF2CAT_WORK%"
  exit /b 1
)

copy /y "%INF2CAT_INF%" "%INF2CAT_WORK%\" >nul
for %%F in (%*) do (
  if not exist "%%F" (
    echo [ERROR] Missing file required by %INF2CAT_INF%: %%F
    exit /b 1
  )
  copy /y "%%F" "%INF2CAT_WORK%\" >nul
)

echo [INFO] Generating %INF2CAT_CAT% (from %INF2CAT_INF%)...
if not defined INF2CAT_OS (
  echo [ERROR] Internal error: INF2CAT_OS is not set.
  exit /b 1
)
inf2cat.exe /driver:"%INF2CAT_WORK%" /os:%INF2CAT_OS% >nul
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] inf2cat.exe failed for %INF2CAT_INF%.
  exit /b 1
)

if not exist "%INF2CAT_WORK%\%INF2CAT_CAT%" (
  echo [ERROR] inf2cat did not produce "%INF2CAT_CAT%" for %INF2CAT_INF%.
  exit /b 1
)

copy /y "%INF2CAT_WORK%\%INF2CAT_CAT%" "%SCRIPT_DIR%" >nul
exit /b 0
