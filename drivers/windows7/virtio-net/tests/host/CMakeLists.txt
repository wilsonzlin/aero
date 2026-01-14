cmake_minimum_required(VERSION 3.16)
project(virtio_net_host_tests C)

enable_testing()

add_executable(virtio_net_hdr_offload_tests
    virtio_net_hdr_offload_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../../src/virtio_net_hdr_offload.c
)

target_include_directories(virtio_net_hdr_offload_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../../include
)

set_target_properties(virtio_net_hdr_offload_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

if (CMAKE_C_COMPILER_ID MATCHES "Clang|GNU")
    target_compile_options(virtio_net_hdr_offload_tests PRIVATE
        -Wall -Wextra -Werror
    )
endif ()

add_test(NAME virtio_net_hdr_offload_tests COMMAND virtio_net_hdr_offload_tests)
