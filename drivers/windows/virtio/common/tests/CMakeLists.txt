add_executable(virtqueue_split_test
    virtqueue_split_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtqueue_split.c
)

set_target_properties(virtqueue_split_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtqueue_split_test COMMAND virtqueue_split_test)

add_executable(virtqueue_split_stress_test
    virtqueue_split_stress_test.c
    ${CMAKE_CURRENT_LIST_DIR}/../virtqueue_split.c
)

target_compile_definitions(virtqueue_split_stress_test PRIVATE
    VIRTQ_DEBUG
)

set_target_properties(virtqueue_split_stress_test PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED YES
)

add_test(NAME virtqueue_split_stress_test COMMAND virtqueue_split_stress_test)
set_tests_properties(virtqueue_split_stress_test PROPERTIES
    TIMEOUT 30
)
