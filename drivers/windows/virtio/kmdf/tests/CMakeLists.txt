if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
    cmake_minimum_required(VERSION 3.15)
    project(virtio_kmdf_tests C)
    enable_testing()
endif()

add_subdirectory(virtio_pci_interrupts_host)

