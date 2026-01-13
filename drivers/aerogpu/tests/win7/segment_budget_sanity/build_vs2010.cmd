@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [segment_budget_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\segment_budget_sanity.exe" user32.lib gdi32.lib setupapi.lib advapi32.lib
if errorlevel 1 exit /b 1

echo [segment_budget_sanity] OK: %OUTDIR%\\segment_budget_sanity.exe
exit /b 0

