cmake_minimum_required(VERSION 3.16)
project(virtio_input_hid_translate_tests C)

enable_testing()

add_executable(hid_translate_test
    hid_translate_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/hid_translate.c
)

set_target_properties(hid_translate_test PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME hid_translate_test COMMAND hid_translate_test)

