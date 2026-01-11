add_executable(virtio_pci_modern_transport_tests
    test_main.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtio_pci_modern_transport.c
    ${CMAKE_CURRENT_LIST_DIR}/../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c
)

target_include_directories(virtio_pci_modern_transport_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/..
)

set_target_properties(virtio_pci_modern_transport_tests PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_modern_transport_tests COMMAND virtio_pci_modern_transport_tests)

