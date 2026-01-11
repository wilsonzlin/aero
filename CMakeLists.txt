cmake_minimum_required(VERSION 3.16)

project(aero_windows7_virtio_common C)

option(AERO_VIRTIO_BUILD_TESTS "Build host-side virtio common tests" ON)

if (AERO_VIRTIO_BUILD_TESTS)
    enable_testing()
    if (EXISTS "${CMAKE_CURRENT_LIST_DIR}/drivers/windows7/virtio/common/tests/CMakeLists.txt")
        add_subdirectory(drivers/windows7/virtio/common/tests)
    endif ()

    if (EXISTS "${CMAKE_CURRENT_LIST_DIR}/drivers/windows7/virtio-modern/common/tests/CMakeLists.txt")
        add_subdirectory(drivers/windows7/virtio-modern/common/tests)
    endif ()

    if (EXISTS "${CMAKE_CURRENT_LIST_DIR}/drivers/windows/virtio/common/tests/CMakeLists.txt")
        add_subdirectory(drivers/windows/virtio/common/tests)
    endif ()
endif ()
