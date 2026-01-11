@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [dump_createalloc_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\dump_createalloc_sanity.exe" user32.lib gdi32.lib
if errorlevel 1 exit /b 1

echo [dump_createalloc_sanity] OK: %OUTDIR%\\dump_createalloc_sanity.exe
exit /b 0
