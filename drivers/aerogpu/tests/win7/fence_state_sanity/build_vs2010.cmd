@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [fence_state_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\fence_state_sanity.exe" user32.lib gdi32.lib
if errorlevel 1 exit /b 1

echo [fence_state_sanity] OK: %OUTDIR%\\fence_state_sanity.exe
exit /b 0

