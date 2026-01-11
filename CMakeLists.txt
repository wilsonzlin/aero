cmake_minimum_required(VERSION 3.16)

project(aero_windows7_virtio_common C)

option(AERO_VIRTIO_BUILD_TESTS "Build host-side virtio common tests" ON)

if (AERO_VIRTIO_BUILD_TESTS)
    enable_testing()
    add_subdirectory(drivers/windows7/virtio/common/tests)
    add_subdirectory(drivers/windows7/virtio-modern/common/tests)
endif ()
