@echo off
setlocal

set "OUTDIR=%~dp0bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [hidtest] Building...

cl /nologo /W4 /O2 /MT /D_CRT_SECURE_NO_WARNINGS "%~dp0main.c" ^
  /link /OUT:"%OUTDIR%\\hidtest.exe" setupapi.lib hid.lib
if errorlevel 1 exit /b 1

echo [hidtest] OK: %OUTDIR%\\hidtest.exe
exit /b 0

