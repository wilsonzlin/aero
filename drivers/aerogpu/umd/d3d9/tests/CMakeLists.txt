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

add_executable(aerogpu_d3d9_draw_requires_shaders_tests
    draw_requires_shaders_tests.cpp
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

target_include_directories(aerogpu_d3d9_draw_requires_shaders_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_draw_requires_shaders_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_draw_requires_shaders_tests COMMAND aerogpu_d3d9_draw_requires_shaders_tests)

add_executable(aerogpu_device_lost_tests
    device_lost_tests.cpp
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

target_include_directories(aerogpu_device_lost_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_device_lost_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_device_lost_tests COMMAND aerogpu_device_lost_tests)

add_executable(aerogpu_d3d9_draw_shader_fallback_tests
    draw_shader_fallback_tests.cpp
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

target_include_directories(aerogpu_d3d9_draw_shader_fallback_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_draw_shader_fallback_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_draw_shader_fallback_tests COMMAND aerogpu_d3d9_draw_shader_fallback_tests)

add_executable(aerogpu_process_vertices_tests
    process_vertices_tests.cpp
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

target_include_directories(aerogpu_process_vertices_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_process_vertices_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_process_vertices_tests COMMAND aerogpu_process_vertices_tests)

add_executable(aerogpu_d3d9_trace_process_vertices_single_record_tests
    trace_process_vertices_single_record_tests.cpp
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

target_include_directories(aerogpu_d3d9_trace_process_vertices_single_record_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_trace_process_vertices_single_record_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_process_vertices_single_record_tests COMMAND aerogpu_d3d9_trace_process_vertices_single_record_tests)

add_executable(aerogpu_d3d9_patch_process_vtable_tests
    device_funcs_patch_process_vertices_tests.cpp
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

target_include_directories(aerogpu_d3d9_patch_process_vtable_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_patch_process_vtable_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_patch_process_vtable_tests COMMAND aerogpu_d3d9_patch_process_vtable_tests)

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

add_executable(aerogpu_shared_surface_fixup_tests
    shared_surface_fixup_tests.cpp
)

target_include_directories(aerogpu_shared_surface_fixup_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
    ${CMAKE_CURRENT_LIST_DIR}/../../d3d10_11/src
)

set_target_properties(aerogpu_shared_surface_fixup_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_shared_surface_fixup_tests COMMAND aerogpu_shared_surface_fixup_tests)

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

add_executable(aerogpu_d3d9_trace_filter_case_insensitive_tests
    trace_filter_case_insensitive_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_case_insensitive_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_case_insensitive_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_case_insensitive_tests COMMAND aerogpu_d3d9_trace_filter_case_insensitive_tests)

add_executable(aerogpu_d3d9_trace_filter_stateblock_token_tests
    trace_filter_stateblock_token_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_stateblock_token_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_stateblock_token_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_stateblock_token_tests COMMAND aerogpu_d3d9_trace_filter_stateblock_token_tests)

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

add_executable(aerogpu_d3d9_trace_filter_commas_only_disables_tests
    trace_filter_commas_only_disables_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_trace.cpp
)

target_include_directories(aerogpu_d3d9_trace_filter_commas_only_disables_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

set_target_properties(aerogpu_d3d9_trace_filter_commas_only_disables_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_trace_filter_commas_only_disables_tests COMMAND aerogpu_d3d9_trace_filter_commas_only_disables_tests)

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

add_executable(aerogpu_feature_decode_tests
    feature_decode_tests.cpp
)

target_include_directories(aerogpu_feature_decode_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_feature_decode_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_feature_decode_tests COMMAND aerogpu_feature_decode_tests)

add_executable(aerogpu_d3d9_format_layout_tests
    format_layout_tests.cpp
)

target_include_directories(aerogpu_d3d9_format_layout_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_format_layout_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_format_layout_tests COMMAND aerogpu_d3d9_format_layout_tests)

add_executable(aerogpu_d3d9_caps_invariants_tests
    caps_invariants_tests.cpp
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

target_include_directories(aerogpu_d3d9_caps_invariants_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_caps_invariants_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_caps_invariants_tests COMMAND aerogpu_d3d9_caps_invariants_tests)

add_executable(aerogpu_d3d9_texture2d_subresource_layout_tests
    texture2d_subresource_layout_tests.cpp
)

target_include_directories(aerogpu_d3d9_texture2d_subresource_layout_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_texture2d_subresource_layout_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_texture2d_subresource_layout_tests COMMAND aerogpu_d3d9_texture2d_subresource_layout_tests)

add_executable(aerogpu_d3d9_fixedfunc_fvf_tests
    fixedfunc_fvf_tests.cpp
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

target_include_directories(aerogpu_d3d9_fixedfunc_fvf_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_fixedfunc_fvf_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_fixedfunc_fvf_tests COMMAND aerogpu_d3d9_fixedfunc_fvf_tests)

add_executable(aerogpu_d3d9_shader_stage_interop_tests
    shader_stage_interop_tests.cpp
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

target_include_directories(aerogpu_d3d9_shader_stage_interop_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d9_shader_stage_interop_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d9_shader_stage_interop_tests COMMAND aerogpu_d3d9_shader_stage_interop_tests)
