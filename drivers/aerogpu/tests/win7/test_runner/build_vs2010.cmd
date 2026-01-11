@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [aerogpu_test_runner] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\aerogpu_test_runner.exe" user32.lib d3d9.lib
if errorlevel 1 exit /b 1

echo [aerogpu_test_runner] OK: %OUTDIR%\\aerogpu_test_runner.exe
exit /b 0
