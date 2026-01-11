# SPDX-License-Identifier: MIT OR Apache-2.0

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
