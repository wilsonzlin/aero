add_executable(virtio_modern_common_tests
    test_main.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/aero_virtio_pci_modern.c
)

target_include_directories(virtio_modern_common_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../include
)

target_compile_definitions(virtio_modern_common_tests PRIVATE
    AERO_VIRTIO_PCI_MODERN_USE_TEST_MMIO
)

set_target_properties(virtio_modern_common_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_modern_common_tests COMMAND virtio_modern_common_tests)

