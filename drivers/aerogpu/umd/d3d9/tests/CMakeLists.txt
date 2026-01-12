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

add_executable(aerogpu_d3d9_trace_env_bool_variants_tests
    trace_env_bool_variants_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_env_bool_variants_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_env_bool_variants_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_env_bool_variants_tests COMMAND aerogpu_d3d9_trace_env_bool_variants_tests)

add_executable(aerogpu_d3d9_trace_filter_unknown_token_tests
    trace_filter_unknown_token_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_unknown_token_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_unknown_token_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_unknown_token_tests COMMAND aerogpu_d3d9_trace_filter_unknown_token_tests)

add_executable(aerogpu_d3d9_trace_filter_whitespace_only_disables_tests
    trace_filter_whitespace_only_disables_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_whitespace_only_disables_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_whitespace_only_disables_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_whitespace_only_disables_tests COMMAND aerogpu_d3d9_trace_filter_whitespace_only_disables_tests)

add_executable(aerogpu_d3d9_trace_filter_stub_token_tests
    trace_filter_stub_token_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_stub_token_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_stub_token_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_stub_token_tests COMMAND aerogpu_d3d9_trace_filter_stub_token_tests)

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

add_executable(aerogpu_d3d9_trace_dump_on_stub_noop_tests
    trace_dump_on_stub_noop_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_stub_noop_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_stub_noop_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_stub_noop_tests COMMAND aerogpu_d3d9_trace_dump_on_stub_noop_tests)

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

add_executable(aerogpu_d3d9_trace_dump_on_stub_filter_stub_tests
    trace_dump_on_stub_filter_stub_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_on_stub_filter_stub_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_on_stub_filter_stub_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_on_stub_filter_stub_tests COMMAND aerogpu_d3d9_trace_dump_on_stub_filter_stub_tests)

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

add_executable(aerogpu_d3d9_trace_dump_present_force_record_tests
    trace_dump_present_force_record_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_present_force_record_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_present_force_record_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_present_force_record_tests COMMAND aerogpu_d3d9_trace_dump_present_force_record_tests)

add_executable(aerogpu_d3d9_trace_dump_present_filtered_tests
    trace_dump_present_filtered_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_present_filtered_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_present_filtered_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_present_filtered_tests COMMAND aerogpu_d3d9_trace_dump_present_filtered_tests)

add_executable(aerogpu_d3d9_trace_dump_present_inflight_hr_tests
    trace_dump_present_inflight_hr_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_dump_present_inflight_hr_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_dump_present_inflight_hr_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_dump_present_inflight_hr_tests COMMAND aerogpu_d3d9_trace_dump_present_inflight_hr_tests)

add_executable(aerogpu_d3d9_trace_unique_mode_tests
    trace_unique_mode_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_unique_mode_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_unique_mode_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_unique_mode_tests COMMAND aerogpu_d3d9_trace_unique_mode_tests)

add_executable(aerogpu_d3d9_trace_mode_all_records_duplicates_tests
    trace_mode_all_records_duplicates_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_mode_all_records_duplicates_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_mode_all_records_duplicates_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_mode_all_records_duplicates_tests COMMAND aerogpu_d3d9_trace_mode_all_records_duplicates_tests)

add_executable(aerogpu_d3d9_trace_mode_default_unique_tests
    trace_mode_default_unique_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_mode_default_unique_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_mode_default_unique_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_mode_default_unique_tests COMMAND aerogpu_d3d9_trace_mode_default_unique_tests)

add_executable(aerogpu_d3d9_trace_mode_invalid_defaults_unique_tests
    trace_mode_invalid_defaults_unique_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_mode_invalid_defaults_unique_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_mode_invalid_defaults_unique_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_mode_invalid_defaults_unique_tests COMMAND aerogpu_d3d9_trace_mode_invalid_defaults_unique_tests)

add_executable(aerogpu_d3d9_trace_max_tests
    trace_max_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_max_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_max_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_max_tests COMMAND aerogpu_d3d9_trace_max_tests)

add_executable(aerogpu_d3d9_trace_max_clamps_to_capacity_tests
    trace_max_clamps_to_capacity_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_max_clamps_to_capacity_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_max_clamps_to_capacity_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_max_clamps_to_capacity_tests COMMAND aerogpu_d3d9_trace_max_clamps_to_capacity_tests)

add_executable(aerogpu_d3d9_trace_max_zero_defaults_tests
    trace_max_zero_defaults_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_max_zero_defaults_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_max_zero_defaults_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_max_zero_defaults_tests COMMAND aerogpu_d3d9_trace_max_zero_defaults_tests)
