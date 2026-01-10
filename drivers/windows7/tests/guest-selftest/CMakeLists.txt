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
  target_compile_options(aero-virtio-selftest PRIVATE /W4)
else()
  target_compile_options(aero-virtio-selftest PRIVATE -Wall -Wextra -Wpedantic)
endif()

target_link_libraries(aero-virtio-selftest PRIVATE
  setupapi
  iphlpapi
  winhttp
  ws2_32
)
