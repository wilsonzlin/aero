add_executable(virtio_pci_interrupts_host_tests
    test_main.c
    ${CMAKE_CURRENT_LIST_DIR}/../../virtio_pci_interrupts.c
)

target_include_directories(virtio_pci_interrupts_host_tests PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}/include
    ${CMAKE_CURRENT_LIST_DIR}/../..
    ${CMAKE_CURRENT_LIST_DIR}/../../../../../win7/virtio/virtio-core/include
)

set_target_properties(virtio_pci_interrupts_host_tests PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtio_pci_interrupts_host_tests COMMAND virtio_pci_interrupts_host_tests)

