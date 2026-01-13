@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Verifies Aero virtio-input driver package signatures.
rem
rem Usage:
rem   verify-signature.cmd [PACKAGE_DIR]
rem
rem PACKAGE_DIR defaults to: inf\
rem
rem Verifies:
rem   signtool verify /kp /v aero_virtio_input.sys
rem   signtool verify /kp /v aero_virtio_input.cat

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI

set PACKAGE_DIR_ARG=%~1

if "%PACKAGE_DIR_ARG%"=="" (
  set PACKAGE_DIR=%ROOT_DIR%\inf
) else (
  rem Treat relative paths as relative to the driver root.
  if "%PACKAGE_DIR_ARG:~1,1%"==":" (
    set PACKAGE_DIR=%PACKAGE_DIR_ARG%
  ) else (
    if "%PACKAGE_DIR_ARG:~0,2%"=="\\" (
      set PACKAGE_DIR=%PACKAGE_DIR_ARG%
    ) else (
      set PACKAGE_DIR=%ROOT_DIR%\%PACKAGE_DIR_ARG%
    )
  )
)

set SYS_FILE=%PACKAGE_DIR%\aero_virtio_input.sys
set CAT_FILE=%PACKAGE_DIR%\aero_virtio_input.cat

if not exist "%PACKAGE_DIR%" (
  echo ERROR: Package directory not found: "%PACKAGE_DIR%"
  exit /b 1
)

if not exist "%SYS_FILE%" (
  echo ERROR: Driver binary not found: "%SYS_FILE%"
  exit /b 1
)

if not exist "%CAT_FILE%" (
  echo ERROR: Catalog not found: "%CAT_FILE%"
  exit /b 1
)

where signtool.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: signtool.exe not found in PATH.
  echo        Install WDK 7.1 or WDK 10 and run this from a WDK Developer Command Prompt.
  exit /b 1
)

echo.
echo == Verifying driver package signatures ==
echo Package: "%PACKAGE_DIR%"
echo.

signtool.exe verify /kp /v "%SYS_FILE%"
if errorlevel 1 (
  echo ERROR: Signature verification failed for SYS: "%SYS_FILE%"
  exit /b 1
)

signtool.exe verify /kp /v "%CAT_FILE%"
if errorlevel 1 (
  echo ERROR: Signature verification failed for CAT: "%CAT_FILE%"
  exit /b 1
)

echo.
echo OK: SYS and CAT signatures verified.
exit /b 0
