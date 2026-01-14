@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [map_shared_handle_stress] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\map_shared_handle_stress.exe" user32.lib gdi32.lib
if errorlevel 1 exit /b 1

echo [map_shared_handle_stress] OK: %OUTDIR%\\map_shared_handle_stress.exe
exit /b 0

