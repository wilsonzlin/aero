@echo off
setlocal

echo === Building AeroGPU Win7 test suite (VS2010) ===

call "%~dp0timeout_runner\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d9ex_dwm_probe\\build_vs2010.cmd" || exit /b 1
call "%~dp0vblank_wait_sanity\\build_vs2010.cmd" || exit /b 1
call "%~dp0wait_vblank_pacing\\build_vs2010.cmd" || exit /b 1
call "%~dp0dwm_flush_pacing\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d9_raster_status_pacing\\build_vs2010.cmd" || exit /b 1
call "%~dp0vblank_wait_pacing\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d9ex_triangle\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d9ex_query_latency\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d9ex_shared_surface\\build_vs2010.cmd" || exit /b 1
call "%~dp0d3d11_triangle\\build_vs2010.cmd" || exit /b 1
call "%~dp0readback_sanity\\build_vs2010.cmd" || exit /b 1

echo.
echo Build complete. Binaries are in: %~dp0bin\
exit /b 0

