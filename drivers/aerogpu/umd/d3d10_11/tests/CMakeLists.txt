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

add_executable(aerogpu_d3d10_11_render_targets_tests
    render_targets_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_log.cpp
)

target_include_directories(aerogpu_d3d10_11_render_targets_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_render_targets_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_render_targets_tests COMMAND aerogpu_d3d10_11_render_targets_tests)

add_executable(aerogpu_d3d10_11_depth_stencil_state_tests
    depth_stencil_state_tests.cpp
)

target_include_directories(aerogpu_d3d10_11_depth_stencil_state_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_depth_stencil_state_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_depth_stencil_state_tests COMMAND aerogpu_d3d10_11_depth_stencil_state_tests)

add_executable(aerogpu_d3d10_11_render_target_tests
    render_target_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_render_target_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_render_target_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_render_target_tests COMMAND aerogpu_d3d10_11_render_target_tests)

add_executable(aerogpu_d3d10_11_viewport_scissor_validation_tests
    viewport_scissor_validation_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_viewport_scissor_validation_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_viewport_scissor_validation_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_viewport_scissor_validation_tests
    COMMAND aerogpu_d3d10_11_viewport_scissor_validation_tests)

add_executable(aerogpu_d3d10_11_mrt_tests
    mrt_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_mrt_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_mrt_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_mrt_tests COMMAND aerogpu_d3d10_11_mrt_tests)

add_executable(aerogpu_d3d10_11_blend_state_validation_tests
    blend_state_validation_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_blend_state_validation_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_blend_state_validation_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_blend_state_validation_tests COMMAND aerogpu_d3d10_11_blend_state_validation_tests)

add_executable(aerogpu_d3d10_11_state_packets_tests
    state_packets_tests.cpp
    ${CMAKE_CURRENT_LIST_DIR}/../src/aerogpu_d3d10_11_umd.cpp
)

target_include_directories(aerogpu_d3d10_11_state_packets_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_state_packets_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_state_packets_tests COMMAND aerogpu_d3d10_11_state_packets_tests)

add_executable(aerogpu_d3d10_11_dxgi_format_tests
    dxgi_format_tests.cpp
)

target_include_directories(aerogpu_d3d10_11_dxgi_format_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_dxgi_format_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_dxgi_format_tests COMMAND aerogpu_d3d10_11_dxgi_format_tests)

add_executable(aerogpu_d3d10_11_gs_shader_packets_tests
    gs_shader_packets_tests.cpp
)

target_include_directories(aerogpu_d3d10_11_gs_shader_packets_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_gs_shader_packets_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_gs_shader_packets_tests COMMAND aerogpu_d3d10_11_gs_shader_packets_tests)

add_executable(aerogpu_d3d10_11_gs_resource_packets_tests
    gs_resource_packets_tests.cpp
)

target_include_directories(aerogpu_d3d10_11_gs_resource_packets_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}/../src
    ${CMAKE_CURRENT_LIST_DIR}/../../../protocol
)

set_target_properties(aerogpu_d3d10_11_gs_resource_packets_tests PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED YES
)

add_test(NAME aerogpu_d3d10_11_gs_resource_packets_tests COMMAND aerogpu_d3d10_11_gs_resource_packets_tests)
