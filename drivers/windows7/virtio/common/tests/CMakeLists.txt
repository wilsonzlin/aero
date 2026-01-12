cmake_minimum_required(VERSION 3.16)
project(virtio_common_tests C)

enable_testing()

add_executable(virtio_common_tests
    test_main.c
    test_os.c
    fake_pci_device.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtqueue_split_legacy.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_pci_legacy.c
)

target_include_directories(virtio_common_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}
)

set_target_properties(virtio_common_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_common_tests COMMAND virtio_common_tests)

add_executable(virtio_pci_cap_parser_tests
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/tests/virtio_pci_cap_parser_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_aero_layout.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_identity.c
)

target_include_directories(virtio_pci_cap_parser_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable
)

set_target_properties(virtio_pci_cap_parser_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_cap_parser_tests COMMAND virtio_pci_cap_parser_tests)

add_executable(virtio_pci_cap_parser_tests_strict_layout
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/tests/virtio_pci_cap_parser_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_aero_layout.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_identity.c
)

target_include_directories(virtio_pci_cap_parser_tests_strict_layout PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable
)

target_compile_definitions(virtio_pci_cap_parser_tests_strict_layout PRIVATE
    VIRTIO_CORE_ENFORCE_AERO_MMIO_LAYOUT=1
)

set_target_properties(virtio_pci_cap_parser_tests_strict_layout PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_cap_parser_tests_strict_layout COMMAND virtio_pci_cap_parser_tests_strict_layout)

add_executable(virtio_intx_wdm_tests
    test_intx_wdm.c
    ${CMAKE_CURRENT_LIST_DIR}/wdk_stubs/wdk_stubs.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_pci_intx_wdm.c
)

target_include_directories(virtio_intx_wdm_tests BEFORE PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/wdk_stubs
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}
)

set_target_properties(virtio_intx_wdm_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_intx_wdm_tests COMMAND virtio_intx_wdm_tests)

add_executable(virtio_pci_modern_miniport_tests
    test_modern_miniport.c
    ${CMAKE_CURRENT_LIST_DIR}/wdk_stubs/wdk_stubs.c
    ${CMAKE_CURRENT_LIST_DIR}/wdk_stubs/virtio_pci_modern_mmio_sim.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_pci_modern_miniport.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c
)

target_include_directories(virtio_pci_modern_miniport_tests BEFORE PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/wdk_stubs
    ${CMAKE_CURRENT_LIST_DIR}/../include
    ${CMAKE_CURRENT_LIST_DIR}
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable
)

set_target_properties(virtio_pci_modern_miniport_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_modern_miniport_tests COMMAND virtio_pci_modern_miniport_tests)
