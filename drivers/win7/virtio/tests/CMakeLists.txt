if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    cmake_minimum_required(VERSION 3.15)
    project(virtio_win7_tests C)
    enable_testing()
endif()

add_executable(virtio_pci_cap_parser_test
    virtio_pci_cap_parser_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtio-core/portable/virtio_pci_cap_parser.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtio-core/portable/virtio_pci_aero_layout.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtio-core/portable/virtio_pci_identity.c
)

target_include_directories(virtio_pci_cap_parser_test PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/../virtio-core/portable
)

set_target_properties(virtio_pci_cap_parser_test PROPERTIES
    C_STANDARD 99
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_cap_parser_test COMMAND virtio_pci_cap_parser_test)
