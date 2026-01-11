# SPDX-License-Identifier: MIT OR Apache-2.0

cmake_minimum_required(VERSION 3.16)

project(aero_virtio_selftest LANGUAGES CXX)

add_executable(aero-virtio-selftest
  src/main.cpp
)

target_compile_features(aero-virtio-selftest PRIVATE cxx_std_17)

target_compile_definitions(aero-virtio-selftest PRIVATE
  UNICODE
  _UNICODE
  WIN32_LEAN_AND_MEAN
  NOMINMAX
  _WIN32_WINNT=0x0601
  WINVER=0x0601
)

if (MSVC)
  # Build a standalone binary that can run on a clean Windows 7 SP1 image
  # without requiring a VC++ redistributable / UCRT install step.
  set_property(TARGET aero-virtio-selftest PROPERTY
    MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
  )

  target_compile_options(aero-virtio-selftest PRIVATE /W4)
  target_link_options(aero-virtio-selftest PRIVATE "/SUBSYSTEM:CONSOLE,6.01")
else()
  target_compile_options(aero-virtio-selftest PRIVATE -Wall -Wextra -Wpedantic)
endif()

target_link_libraries(aero-virtio-selftest PRIVATE
  setupapi
  cfgmgr32
  iphlpapi
  winmm
  winhttp
  ws2_32
)
