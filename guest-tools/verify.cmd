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
REM
REM Optional PowerShell parameters (forwarded to verify.ps1):
REM   verify.cmd -PingTarget 192.168.0.1
REM   verify.cmd -PlayTestSound
REM   verify.cmd -RunDbgctl
REM   verify.cmd -RunDbgctlSelftest

set "SCRIPT_DIR=%~dp0"
set "PS_SCRIPT=%SCRIPT_DIR%verify.ps1"
set "PS_EXE=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
if defined PROCESSOR_ARCHITEW6432 set "PS_EXE=%SystemRoot%\Sysnative\WindowsPowerShell\v1.0\powershell.exe"
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"

if not exist "%PS_SCRIPT%" (
  echo ERROR: Missing "%PS_SCRIPT%".
  exit /b 2
)

REM Quick elevation hint. (This does not auto-elevate; it only warns.)
"%SYS32%\fsutil.exe" dirty query %SystemDrive% >nul 2>&1
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
echo Optional dbgctl artifacts (if aerogpu_dbgctl.exe is present on the Guest Tools media):
echo   C:\AeroGuestTools\dbgctl_version.txt   (best-effort: runs a safe --version or /? with timeout)
echo   C:\AeroGuestTools\dbgctl_status.txt    (only when -RunDbgctl is used and AeroGPU is healthy)
echo   C:\AeroGuestTools\dbgctl_selftest.txt  (only when -RunDbgctlSelftest is used and AeroGPU is healthy)
echo   Note: Optional tools inventory (tools\*) is included in report.txt under "Optional Tools (tools\*)".
exit /b %EXITCODE%

