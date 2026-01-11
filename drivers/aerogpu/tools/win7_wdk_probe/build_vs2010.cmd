@echo off
setlocal

set "OUTDIR=%~dp0bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [win7_wdk_probe] Building...

rem
rem This tool is expected to be built inside a Win7-era WDK/VS environment where
rem the WDDM/D3D UMD headers are available on the include path.
rem

cl /nologo /W4 /EHsc /O2 /MT /DUNICODE /D_UNICODE ^
  "%~dp0src\\win7_wdk_probe.cpp" ^
  /link /OUT:"%OUTDIR%\\win7_wdk_probe.exe"
if errorlevel 1 exit /b 1

echo [win7_wdk_probe] OK: %OUTDIR%\\win7_wdk_probe.exe
exit /b 0

