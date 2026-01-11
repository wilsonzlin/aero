cmake_minimum_required(VERSION 3.16)
project(virtio_common_tests C)

enable_testing()

add_executable(virtio_common_tests
    test_main.c
    test_os.c
    fake_pci_device.c
    fake_pci_device_modern.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtqueue_split.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_pci_legacy.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtio_pci_modern.c
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
