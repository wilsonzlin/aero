cmake_minimum_required(VERSION 3.16)
project(aero_virtio_net_tests C)

enable_testing()

add_executable(aero_virtio_net_offload_tests
    test_offload.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/aero_virtio_net_offload.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_net_hdr_offload.c
)

target_include_directories(aero_virtio_net_offload_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aero_virtio_net_offload_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

if (CMAKE_C_COMPILER_ID MATCHES "Clang|GNU")
    target_compile_options(aero_virtio_net_offload_tests PRIVATE
        -Wall -Wextra -Werror
    )
endif ()

add_test(NAME aero_virtio_net_offload_tests COMMAND aero_virtio_net_offload_tests)

if (EXISTS "${CMAKE_CURRENT_LIST_DIR}/host/CMakeLists.txt")
    add_subdirectory(host)
endif ()
