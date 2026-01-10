@echo off
setlocal

set "OUTDIR=%~dp0bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [aerogpu_dbgctl] Building...

cl /nologo /W4 /EHsc /O2 /MT /DUNICODE /D_UNICODE ^
  /I "%~dp0..\\..\\protocol" ^
  "%~dp0src\\aerogpu_dbgctl.cpp" ^
  /link /OUT:"%OUTDIR%\\aerogpu_dbgctl.exe" user32.lib gdi32.lib
if errorlevel 1 exit /b 1

echo [aerogpu_dbgctl] OK: %OUTDIR%\\aerogpu_dbgctl.exe
exit /b 0

