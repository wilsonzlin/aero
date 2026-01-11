# SPDX-License-Identifier: MIT OR Apache-2.0

cmake_minimum_required(VERSION 3.15)
project(virtiosnd_host_tests C)

enable_testing()

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED YES)

if (CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
    add_compile_options(-Wno-multichar)
endif()

set(VIRTIOSND_ROOT ${CMAKE_CURRENT_LIST_DIR}/../..)
set(VIRTIOSND_SRC ${VIRTIOSND_ROOT}/src)
set(VIRTIOSND_INCLUDE ${VIRTIOSND_ROOT}/include)
set(VIRTIO_COMMON_INCLUDE ${VIRTIOSND_ROOT}/../virtio/common/include)

add_library(virtiosnd_host_shim STATIC
    shim/virtiosnd_dma_stub.c
    shim/virtiosnd_host_queue.c
)

target_include_directories(virtiosnd_host_shim PUBLIC
    ${CMAKE_CURRENT_LIST_DIR}/shim
    ${VIRTIOSND_INCLUDE}
)

add_executable(virtiosnd_control_proto_tests
    test_control_proto.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_control_proto.c
)

target_link_libraries(virtiosnd_control_proto_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_control_proto_tests COMMAND virtiosnd_control_proto_tests)

add_executable(virtiosnd_tx_tests
    test_tx_engine.c
    ${VIRTIOSND_SRC}/virtiosnd_tx.c
)

target_link_libraries(virtiosnd_tx_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_tx_tests COMMAND virtiosnd_tx_tests)

add_executable(virtiosnd_rx_tests
    test_rx_engine.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_rx.c
)

target_link_libraries(virtiosnd_rx_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_rx_tests COMMAND virtiosnd_rx_tests)

# Existing scatter/gather core tests (host-buildable).
add_executable(virtiosnd_sg_tests
    ${VIRTIOSND_ROOT}/tests/test_sg.c
    ${VIRTIOSND_SRC}/virtiosnd_sg_core.c
)

target_include_directories(virtiosnd_sg_tests PRIVATE
    ${VIRTIOSND_INCLUDE}
    ${VIRTIO_COMMON_INCLUDE}
)

add_test(NAME virtiosnd_sg_tests COMMAND virtiosnd_sg_tests)
