cmake_minimum_required(VERSION 3.16)

if (CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    project(aerogpu_d3d9_tests CXX)
    enable_testing()
endif ()

add_executable(aerogpu_cmd_stream_writer_tests
    cmd_stream_writer_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d9_blit.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d9_caps.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d9_driver.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_kmd_query.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_log.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_alloc.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_alloc_list.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_context.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/dllmain.cpp
)

target_include_directories(aerogpu_cmd_stream_writer_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_cmd_stream_writer_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_cmd_stream_writer_tests COMMAND aerogpu_cmd_stream_writer_tests)

add_executable(aerogpu_alloc_list_tracker_tests
    alloc_list_tracker_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_alloc_list.cpp
)

target_include_directories(aerogpu_alloc_list_tracker_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_alloc_list_tracker_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_alloc_list_tracker_tests COMMAND aerogpu_alloc_list_tracker_tests)

add_executable(aerogpu_dma_priv_tests
    dma_priv_tests.cpp
)

target_include_directories(aerogpu_dma_priv_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_dma_priv_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_dma_priv_tests COMMAND aerogpu_dma_priv_tests)

add_executable(aerogpu_wddm_submit_buffer_utils_tests
    wddm_submit_buffer_utils_tests.cpp
)

target_include_directories(aerogpu_wddm_submit_buffer_utils_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
)

set_target_properties(aerogpu_wddm_submit_buffer_utils_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_wddm_submit_buffer_utils_tests COMMAND aerogpu_wddm_submit_buffer_utils_tests)

add_executable(aerogpu_d3d9_trace_filter_tests
    trace_filter_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_tests COMMAND aerogpu_d3d9_trace_filter_tests)

add_executable(aerogpu_d3d9_trace_dump_on_fail_tests
    trace_dump_on_fail_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_fail_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_fail_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_fail_tests COMMAND aerogpu_d3d9_trace_dump_on_fail_tests)

add_executable(aerogpu_d3d9_trace_dump_on_fail_filtered_tests
    trace_dump_on_fail_filtered_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_fail_filtered_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_fail_filtered_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_fail_filtered_tests COMMAND aerogpu_d3d9_trace_dump_on_fail_filtered_tests)

add_executable(aerogpu_d3d9_trace_dump_on_stub_tests
    trace_dump_on_stub_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_stub_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_stub_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_stub_tests COMMAND aerogpu_d3d9_trace_dump_on_stub_tests)

add_executable(aerogpu_d3d9_trace_dump_on_stub_filtered_tests
    trace_dump_on_stub_filtered_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_stub_filtered_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_stub_filtered_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_stub_filtered_tests COMMAND aerogpu_d3d9_trace_dump_on_stub_filtered_tests)

add_executable(aerogpu_d3d9_trace_dump_present_tests
    trace_dump_present_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_present_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_present_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_present_tests COMMAND aerogpu_d3d9_trace_dump_present_tests)
