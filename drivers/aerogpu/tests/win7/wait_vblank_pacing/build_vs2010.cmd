@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [wait_vblank_pacing] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\wait_vblank_pacing.exe" user32.lib
if errorlevel 1 exit /b 1

echo [wait_vblank_pacing] OK: %OUTDIR%\\wait_vblank_pacing.exe
exit /b 0

