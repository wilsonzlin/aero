# SPDX-License-Identifier: MIT OR Apache-2.0

# This CMake file is designed to work in two modes:
#  - Standalone: `cmake -S drivers/windows7/virtio-snd/tests/host -B ...`
#  - As a subdirectory of `drivers/windows7/virtio-snd/tests` (CI path)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    cmake_minimum_required(VERSION 3.15)
    project(virtiosnd_host_tests C)
    enable_testing()

    message(STATUS "Note: configuring drivers/windows7/virtio-snd/tests/host standalone builds a subset of virtio-snd host tests (does not include virtiosnd_proto_tests). For the full host-buildable suite, configure drivers/windows7/virtio-snd/tests instead.")
endif()

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
    shim/ntddk_shim_globals.c
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

add_executable(virtiosnd_pcm_selection_tests
    test_pcm_selection.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_control_proto.c
)

target_link_libraries(virtiosnd_pcm_selection_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_pcm_selection_tests COMMAND virtiosnd_pcm_selection_tests)

add_executable(virtiosnd_control_engine_tests
    test_control_engine.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_control_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_control.c
)

target_link_libraries(virtiosnd_control_engine_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_control_engine_tests COMMAND virtiosnd_control_engine_tests)

add_executable(virtiosnd_contract_tests
    test_contract.c
    ${VIRTIOSND_SRC}/virtiosnd_contract.c
)

target_link_libraries(virtiosnd_contract_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_contract_tests COMMAND virtiosnd_contract_tests)

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

add_executable(virtiosnd_jack_tests
    test_jack.c
    ${VIRTIOSND_SRC}/virtiosnd_jack.c
)

target_link_libraries(virtiosnd_jack_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_jack_tests COMMAND virtiosnd_jack_tests)

add_executable(virtiosnd_eventq_tests
    test_eventq.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
)

target_link_libraries(virtiosnd_eventq_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_eventq_tests COMMAND virtiosnd_eventq_tests)

add_executable(virtiosnd_dma_stub_tests
    test_dma_stub.c
)

target_link_libraries(virtiosnd_dma_stub_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_dma_stub_tests COMMAND virtiosnd_dma_stub_tests)

add_executable(virtiosnd_event_proto_tests
    test_event_proto.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
)

target_link_libraries(virtiosnd_event_proto_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_event_proto_tests COMMAND virtiosnd_event_proto_tests)

add_executable(virtiosnd_eventq_drain_tests
    test_eventq_drain.c
    ${VIRTIOSND_SRC}/virtio_snd_proto.c
    ${VIRTIOSND_SRC}/virtiosnd_eventq.c
    ${VIRTIOSND_SRC}/virtiosnd_jack.c
)

target_link_libraries(virtiosnd_eventq_drain_tests PRIVATE virtiosnd_host_shim)
add_test(NAME virtiosnd_eventq_drain_tests COMMAND virtiosnd_eventq_drain_tests)

# Existing scatter/gather core tests (host-buildable).
if (NOT TARGET virtiosnd_sg_tests)
    add_executable(virtiosnd_sg_tests
        ${VIRTIOSND_ROOT}/tests/test_sg.c
        ${VIRTIOSND_SRC}/virtiosnd_sg_core.c
    )

    target_include_directories(virtiosnd_sg_tests PRIVATE
        ${VIRTIOSND_INCLUDE}
        ${VIRTIO_COMMON_INCLUDE}
    )

    add_test(NAME virtiosnd_sg_tests COMMAND virtiosnd_sg_tests)
endif()
