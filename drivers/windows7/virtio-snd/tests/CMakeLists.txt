# SPDX-License-Identifier: MIT OR Apache-2.0

cmake_minimum_required(VERSION 3.15)
project(virtiosnd_sg_tests C)

enable_testing()

add_executable(virtiosnd_sg_tests
    test_sg.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtiosnd_sg_core.c
)

target_include_directories(virtiosnd_sg_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../virtio/common/include
)

set_target_properties(virtiosnd_sg_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtiosnd_sg_tests COMMAND virtiosnd_sg_tests)

add_executable(virtiosnd_proto_tests
    test_proto.c
    test_queue.c
    test_dma.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_snd_proto.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtiosnd_control.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtiosnd_tx.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtiosnd_rx.c
)

target_include_directories(virtiosnd_proto_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR} # ntddk.h shim + test helpers
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(virtiosnd_proto_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtiosnd_proto_tests COMMAND virtiosnd_proto_tests)
