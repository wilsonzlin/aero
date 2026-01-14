cmake_minimum_required(VERSION 3.16)
project(virtio_input_tests C)

enable_testing()

set(VIOINPUT_TEST_CFLAGS "")

# Keep assertions enabled even in Release builds.
#
# - CMake defaults to defining NDEBUG in Release, which compiles out assert().
# - For clang-cl, CMAKE_C_COMPILER_ID is "Clang" but the frontend is MSVC; prefer
#   checking the frontend variant over compiler ID matching.
if (CMAKE_C_COMPILER_FRONTEND_VARIANT STREQUAL "MSVC")
    set(VIOINPUT_TEST_CFLAGS /UNDEBUG)
elseif (CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
    set(VIOINPUT_TEST_CFLAGS -Wall -Wextra -Werror -UNDEBUG)
endif ()

add_executable(hid_translate_test
    hid_translate_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/hid_translate.c
)

set_target_properties(hid_translate_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(hid_translate_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME hid_translate_test COMMAND hid_translate_test)

add_executable(led_translate_test
    led_translate_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/led_translate.c
)

set_target_properties(led_translate_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(led_translate_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME led_translate_test COMMAND led_translate_test)

add_executable(led_report_parse_test
    led_report_parse_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/led_report_parse.c
)

set_target_properties(led_report_parse_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(led_report_parse_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME led_report_parse_test COMMAND led_report_parse_test)

add_executable(led_output_pipeline_test
    led_output_pipeline_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/led_report_parse.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/led_translate.c
)

set_target_properties(led_output_pipeline_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(led_output_pipeline_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME led_output_pipeline_test COMMAND led_output_pipeline_test)

add_executable(report_ring_test
    report_ring_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_input.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/hid_translate.c
)

set_target_properties(report_ring_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(report_ring_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME report_ring_test COMMAND report_ring_test)

add_executable(virtio_input_integration_test
    virtio_input_integration_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_input.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/hid_translate.c
)

set_target_properties(virtio_input_integration_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(virtio_input_integration_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME virtio_input_integration_test COMMAND virtio_input_integration_test)

add_executable(virtio_statusq_test
    virtio_statusq_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_statusq.c
)

set_target_properties(virtio_statusq_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
    C_EXTENSIONS NO
)

target_compile_options(virtio_statusq_test PRIVATE ${VIOINPUT_TEST_CFLAGS})
add_test(NAME virtio_statusq_test COMMAND virtio_statusq_test)
