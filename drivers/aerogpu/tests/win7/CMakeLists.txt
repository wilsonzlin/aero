cmake_minimum_required(VERSION 3.16)

project(aerogpu_win7_validation_suite LANGUAGES CXX)

# These binaries are intended to run inside a Windows 7 guest VM. Keep the codebase
# conservative (C++11) so it remains buildable with older MSVC toolsets (including
# v141_xp) while still being easy to build on modern hosts.
set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(MSVC)
  # Match the original VS2010-era build scripts: /W4 /EHsc and a static CRT (/MT).
  add_compile_options(/W4 /EHsc)
  add_compile_definitions(_CRT_SECURE_NO_WARNINGS)

  # CMake 3.15+: prefer the target-level property, but keep a global default.
  if(NOT DEFINED CMAKE_MSVC_RUNTIME_LIBRARY)
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
  endif()
endif()

# Keep the legacy layout: drop binaries next to run_all.cmd in win7/bin/.
set(AEROGPU_TEST_BIN_DIR "${CMAKE_CURRENT_LIST_DIR}/bin")

function(aerogpu_add_win7_test name)
  add_executable(${name} ${ARGN})
  target_include_directories(${name} PRIVATE "${CMAKE_CURRENT_LIST_DIR}/common")

  # Ensure a predictable output path even for multi-config generators (VS).
  set_target_properties(${name} PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${AEROGPU_TEST_BIN_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG "${AEROGPU_TEST_BIN_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${AEROGPU_TEST_BIN_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_RELWITHDEBINFO "${AEROGPU_TEST_BIN_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_MINSIZEREL "${AEROGPU_TEST_BIN_DIR}"
  )

  if(MSVC)
    set_property(TARGET ${name} PROPERTY
      MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
  endif()
endfunction()

aerogpu_add_win7_test(d3d9ex_dwm_probe
  d3d9ex_dwm_probe/main.cpp
)
target_link_libraries(d3d9ex_dwm_probe PRIVATE user32 dwmapi)

aerogpu_add_win7_test(d3d9ex_event_query
  d3d9ex_event_query/main.cpp
)
target_link_libraries(d3d9ex_event_query PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_dwm_ddi_sanity
  d3d9ex_dwm_ddi_sanity/main.cpp
)
target_link_libraries(d3d9ex_dwm_ddi_sanity PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_submit_fence_stress
  d3d9ex_submit_fence_stress/main.cpp
)
target_link_libraries(d3d9ex_submit_fence_stress PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(device_state_sanity
  device_state_sanity/main.cpp
)
target_link_libraries(device_state_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(fence_state_sanity
  fence_state_sanity/main.cpp
)
target_link_libraries(fence_state_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(ring_state_sanity
  ring_state_sanity/main.cpp
)
target_link_libraries(ring_state_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(vblank_wait_sanity
  vblank_wait_sanity/main.cpp
)
target_link_libraries(vblank_wait_sanity PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(vblank_wait
  vblank_wait/main.cpp
)
target_link_libraries(vblank_wait PRIVATE user32 gdi32)

aerogpu_add_win7_test(wait_vblank_pacing
  wait_vblank_pacing/main.cpp
)
target_link_libraries(wait_vblank_pacing PRIVATE user32 gdi32)

aerogpu_add_win7_test(vblank_wait_pacing
  vblank_wait_pacing/main.cpp
)
target_link_libraries(vblank_wait_pacing PRIVATE user32 gdi32)

aerogpu_add_win7_test(vblank_state_sanity
  vblank_state_sanity/main.cpp
)
target_link_libraries(vblank_state_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(get_scanline_sanity
  get_scanline_sanity/main.cpp
)
target_link_libraries(get_scanline_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(scanout_state_sanity
  scanout_state_sanity/main.cpp
)
target_link_libraries(scanout_state_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(dump_createalloc_sanity
  dump_createalloc_sanity/main.cpp
)
target_link_libraries(dump_createalloc_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(umd_private_sanity
  umd_private_sanity/main.cpp
)
target_link_libraries(umd_private_sanity PRIVATE user32 gdi32)

aerogpu_add_win7_test(d3d9_raster_status_sanity
  d3d9_raster_status_sanity/main.cpp
)
target_link_libraries(d3d9_raster_status_sanity PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9_raster_status_pacing
  d3d9_raster_status_pacing/main.cpp
)
target_link_libraries(d3d9_raster_status_pacing PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(dwm_flush_pacing
  dwm_flush_pacing/main.cpp
)
target_link_libraries(dwm_flush_pacing PRIVATE user32 dwmapi)

aerogpu_add_win7_test(d3d9ex_triangle
  d3d9ex_triangle/main.cpp
)
target_link_libraries(d3d9ex_triangle PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_stateblock_sanity
  d3d9ex_stateblock_sanity/main.cpp
)
target_link_libraries(d3d9ex_stateblock_sanity PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_multiframe_triangle
  d3d9ex_multiframe_triangle/main.cpp
)
target_link_libraries(d3d9ex_multiframe_triangle PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_vb_dirty_range
  d3d9ex_vb_dirty_range/main.cpp
)
target_link_libraries(d3d9ex_vb_dirty_range PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_stretchrect
  d3d9ex_stretchrect/main.cpp
)
target_link_libraries(d3d9ex_stretchrect PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_query_latency
  d3d9ex_query_latency/main.cpp
)
target_link_libraries(d3d9ex_query_latency PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_shared_surface
  d3d9ex_shared_surface/main.cpp
)
target_link_libraries(d3d9ex_shared_surface PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_shared_surface_ipc
  d3d9ex_shared_surface_ipc/main.cpp
)
target_link_libraries(d3d9ex_shared_surface_ipc PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_alloc_id_persistence
  d3d9ex_alloc_id_persistence/main.cpp
)
target_link_libraries(d3d9ex_alloc_id_persistence PRIVATE user32 gdi32 d3d9)

if(CMAKE_SIZEOF_VOID_P EQUAL 4)
  aerogpu_add_win7_test(d3d9ex_shared_surface_wow64
    d3d9ex_shared_surface_wow64/producer_main.cpp
  )
  target_link_libraries(d3d9ex_shared_surface_wow64 PRIVATE user32 gdi32 d3d9)

  set(AEROGPU_WIN7_BUILD_WOW64_CONSUMER_DEFAULT OFF)
  if(MSVC AND CMAKE_GENERATOR MATCHES "Visual Studio" AND CMAKE_HOST_SYSTEM_PROCESSOR MATCHES "AMD64|x86_64")
    set(AEROGPU_WIN7_BUILD_WOW64_CONSUMER_DEFAULT ON)
  endif()
  option(AEROGPU_WIN7_BUILD_WOW64_CONSUMER
         "Build the x64 consumer helper binary required by d3d9ex_shared_surface_wow64"
         ${AEROGPU_WIN7_BUILD_WOW64_CONSUMER_DEFAULT})

  if(AEROGPU_WIN7_BUILD_WOW64_CONSUMER AND MSVC AND CMAKE_GENERATOR MATCHES "Visual Studio")
    set(AEROGPU_WIN7_WOW64_CONSUMER_BIN
        "${AEROGPU_TEST_BIN_DIR}/d3d9ex_shared_surface_wow64_consumer_x64.exe")
    set(AEROGPU_WIN7_WOW64_CONSUMER_BUILD_DIR "${CMAKE_CURRENT_BINARY_DIR}/wow64_consumer_x64")
    set(AEROGPU_WIN7_WOW64_CONSUMER_CMAKE_ARGS
        -G "${CMAKE_GENERATOR}"
        -A x64
    )
    if(CMAKE_GENERATOR_TOOLSET)
      list(APPEND AEROGPU_WIN7_WOW64_CONSUMER_CMAKE_ARGS -T "${CMAKE_GENERATOR_TOOLSET}")
    endif()

    add_custom_command(
      OUTPUT "${AEROGPU_WIN7_WOW64_CONSUMER_BIN}"
      COMMAND "${CMAKE_COMMAND}"
              -S "${CMAKE_CURRENT_LIST_DIR}"
              -B "${AEROGPU_WIN7_WOW64_CONSUMER_BUILD_DIR}"
              ${AEROGPU_WIN7_WOW64_CONSUMER_CMAKE_ARGS}
      COMMAND "${CMAKE_COMMAND}"
              --build "${AEROGPU_WIN7_WOW64_CONSUMER_BUILD_DIR}"
              --config $<CONFIG>
              --target d3d9ex_shared_surface_wow64_consumer_x64
      DEPENDS
        "${CMAKE_CURRENT_LIST_FILE}"
        "${CMAKE_CURRENT_LIST_DIR}/d3d9ex_shared_surface_wow64/consumer_main.cpp"
        "${CMAKE_CURRENT_LIST_DIR}/d3d9ex_shared_surface_wow64/wow64_shared_surface_common.h"
      COMMENT "Building d3d9ex_shared_surface_wow64_consumer_x64.exe"
      VERBATIM
    )
    add_custom_target(d3d9ex_shared_surface_wow64_consumer_x64
      DEPENDS "${AEROGPU_WIN7_WOW64_CONSUMER_BIN}"
    )
    add_dependencies(d3d9ex_shared_surface_wow64 d3d9ex_shared_surface_wow64_consumer_x64)
  endif()
elseif(CMAKE_SIZEOF_VOID_P EQUAL 8)
  # Consumer helper binary (x64 only). Build the producer in a Win32 configuration.
  aerogpu_add_win7_test(d3d9ex_shared_surface_wow64_consumer_x64
    d3d9ex_shared_surface_wow64/consumer_main.cpp
  )
  target_link_libraries(d3d9ex_shared_surface_wow64_consumer_x64 PRIVATE user32 gdi32 d3d9)
endif()

aerogpu_add_win7_test(d3d9ex_shared_surface_many_producers
  d3d9ex_shared_surface_many_producers/main.cpp
)
target_link_libraries(d3d9ex_shared_surface_many_producers PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_shared_allocations
  d3d9ex_shared_allocations/main.cpp
)
target_link_libraries(d3d9ex_shared_allocations PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d9ex_shared_surface_stress
  d3d9ex_shared_surface_stress/main.cpp
)
target_link_libraries(d3d9ex_shared_surface_stress PRIVATE user32 gdi32 d3d9)

aerogpu_add_win7_test(d3d10_triangle
  d3d10_triangle/main.cpp
)
target_link_libraries(d3d10_triangle PRIVATE user32 gdi32 d3d10 dxgi)

aerogpu_add_win7_test(d3d10_map_do_not_wait
  d3d10_map_do_not_wait/main.cpp
)
target_link_libraries(d3d10_map_do_not_wait PRIVATE d3d10 dxgi)

aerogpu_add_win7_test(d3d10_shared_surface_ipc
  d3d10_shared_surface_ipc/main.cpp
)
target_link_libraries(d3d10_shared_surface_ipc PRIVATE d3d10 dxgi)

aerogpu_add_win7_test(d3d10_1_triangle
  d3d10_1_triangle/main.cpp
)
target_link_libraries(d3d10_1_triangle PRIVATE user32 gdi32 d3d10_1 d3d10 dxgi)

aerogpu_add_win7_test(d3d10_1_map_do_not_wait
  d3d10_1_map_do_not_wait/main.cpp
)
target_link_libraries(d3d10_1_map_do_not_wait PRIVATE d3d10_1 d3d10 dxgi)

aerogpu_add_win7_test(d3d10_caps_smoke
  d3d10_caps_smoke/main.cpp
)
target_link_libraries(d3d10_caps_smoke PRIVATE d3d10 dxgi)

aerogpu_add_win7_test(d3d11_triangle
  d3d11_triangle/main.cpp
)
target_link_libraries(d3d11_triangle PRIVATE user32 gdi32 d3d11 dxgi)

aerogpu_add_win7_test(d3d11_map_do_not_wait
  d3d11_map_do_not_wait/main.cpp
)
target_link_libraries(d3d11_map_do_not_wait PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_texture
  d3d11_texture/main.cpp
)
target_link_libraries(d3d11_texture PRIVATE user32 gdi32 d3d11 dxgi)

aerogpu_add_win7_test(d3d11_caps_smoke
  d3d11_caps_smoke/main.cpp
)
target_link_libraries(d3d11_caps_smoke PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_rs_om_state_sanity
  d3d11_rs_om_state_sanity/main.cpp
)
target_link_libraries(d3d11_rs_om_state_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_geometry_shader_smoke
  d3d11_geometry_shader_smoke/main.cpp
)
target_link_libraries(d3d11_geometry_shader_smoke PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(dxgi_swapchain_probe
  dxgi_swapchain_probe/main.cpp
)
target_link_libraries(dxgi_swapchain_probe PRIVATE user32 gdi32 d3d11 d3d10_1 d3d10 dxgi)

aerogpu_add_win7_test(d3d11_swapchain_rotate_sanity
  d3d11_swapchain_rotate_sanity/main.cpp
)
target_link_libraries(d3d11_swapchain_rotate_sanity PRIVATE user32 gdi32 d3d11 dxgi)

aerogpu_add_win7_test(d3d11_map_dynamic_buffer_sanity
  d3d11_map_dynamic_buffer_sanity/main.cpp
)
target_link_libraries(d3d11_map_dynamic_buffer_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_map_roundtrip
  d3d11_map_roundtrip/main.cpp
)
target_link_libraries(d3d11_map_roundtrip PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_update_subresource_texture_sanity
  d3d11_update_subresource_texture_sanity/main.cpp
)
target_link_libraries(d3d11_update_subresource_texture_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_shared_surface_ipc
  d3d11_shared_surface_ipc/main.cpp
)
target_link_libraries(d3d11_shared_surface_ipc PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_texture_sampling_sanity
  d3d11_texture_sampling_sanity/main.cpp
)
target_link_libraries(d3d11_texture_sampling_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_dynamic_constant_buffer_sanity
  d3d11_dynamic_constant_buffer_sanity/main.cpp
)
target_link_libraries(d3d11_dynamic_constant_buffer_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(d3d11_depth_test_sanity
  d3d11_depth_test_sanity/main.cpp
)
target_link_libraries(d3d11_depth_test_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(readback_sanity
  readback_sanity/main.cpp
)
target_link_libraries(readback_sanity PRIVATE d3d11 dxgi)

aerogpu_add_win7_test(aerogpu_timeout_runner
  timeout_runner/main.cpp
)

aerogpu_add_win7_test(aerogpu_test_runner
  test_runner/main.cpp
)
target_link_libraries(aerogpu_test_runner PRIVATE user32 d3d9)
