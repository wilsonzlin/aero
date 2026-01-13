@echo off
setlocal

REM Convenience wrapper for users who prefer running from cmd.exe:
REM   inject-driver.cmd -WimPath C:\win7\sources\install.wim -Index 1 -DriverDir C:\pkg\x64
REM
REM This passes all arguments through to the PowerShell script.

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0inject-driver.ps1" %*
exit /b %ERRORLEVEL%

