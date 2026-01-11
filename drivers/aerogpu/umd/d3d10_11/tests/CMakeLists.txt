cmake_minimum_required(VERSION 3.16)

if (CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    project(aerogpu_d3d10_11_tests CXX)
    enable_testing()
endif ()

add_executable(aerogpu_d3d10_11_map_unmap_tests
    map_unmap_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_map_unmap_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_map_unmap_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_map_unmap_tests COMMAND aerogpu_d3d10_11_map_unmap_tests)

