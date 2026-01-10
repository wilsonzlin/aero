add_executable(virtio_common_tests
    test_main.c
    test_os.c
    fake_pci_device.c
    ${CMAKE_CURRENT_LIST_DIR}/../src/virtqueue_split.c
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

