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
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_context.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/dllmain.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_wddm_alloc_list.cpp
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
