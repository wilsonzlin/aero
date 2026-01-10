@echo off
setlocal EnableExtensions EnableDelayedExpansion

REM Aero Guest Tools - Windows 7 in-guest diagnostics + verification
REM
REM This script runs offline and produces:
REM   C:\AeroGuestTools\report.json
REM   C:\AeroGuestTools\report.txt
REM
REM Notes:
REM - Run as Administrator for full results (bcdedit, driver/service queries, output dir).
REM - Uses only built-in Windows 7 tools (cmd + PowerShell + WMI + pnputil + bcdedit).

set "SCRIPT_DIR=%~dp0"
set "PS_SCRIPT=%SCRIPT_DIR%verify.ps1"
set "PS_EXE=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if defined PROCESSOR_ARCHITEW6432 set "PS_EXE=%SystemRoot%\Sysnative\WindowsPowerShell\v1.0\powershell.exe"

if not exist "%PS_SCRIPT%" (
  echo ERROR: Missing "%PS_SCRIPT%".
  exit /b 2
)

REM Quick elevation hint. (This does not auto-elevate; it only warns.)
net session >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo WARNING: Not running elevated. Right-click ^> "Run as administrator" for full checks.
  echo.
)

"%PS_EXE%" -NoProfile -ExecutionPolicy Bypass -File "%PS_SCRIPT%" %*
set "EXITCODE=%ERRORLEVEL%"

echo.
if "%EXITCODE%"=="0" (
  echo Overall: PASS
) else if "%EXITCODE%"=="1" (
  echo Overall: WARN
) else (
  echo Overall: FAIL
)
echo Reports:
echo   C:\AeroGuestTools\report.json
echo   C:\AeroGuestTools\report.txt
exit /b %EXITCODE%

