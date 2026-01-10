@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [timeout_runner] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\aerogpu_timeout_runner.exe"
if errorlevel 1 exit /b 1

echo [timeout_runner] OK: %OUTDIR%\\aerogpu_timeout_runner.exe
exit /b 0

