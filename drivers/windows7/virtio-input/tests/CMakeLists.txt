cmake_minimum_required(VERSION 3.16)
project(virtio_input_tests C)

enable_testing()

set(VIOINPUT_SRC_DIR ${CMAKE_CURRENT_LIST_DIR}/../src)

set(VIOINPUT_TEST_CFLAGS "")

# Keep assertions enabled even in Release builds.
#
# - CMake defaults to defining NDEBUG in Release, which compiles out assert().
# - For clang-cl, CMAKE_C_COMPILER_ID is "Clang" but the frontend is MSVC; prefer
#   checking the frontend variant over compiler ID matching.
if (CMAKE_C_COMPILER_FRONTEND_VARIANT STREQUAL "MSVC")
    set(VIOINPUT_TEST_CFLAGS /UNDEBUG)
elseif (CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
    set(VIOINPUT_TEST_CFLAGS -Wall -Wextra -Werror -UNDEBUG)
endif ()

function(vioinput_list_append_unique list_var)
    foreach(item IN LISTS ARGN)
        list(FIND ${list_var} "${item}" idx)
        if (idx EQUAL -1)
            list(APPEND ${list_var} "${item}")
        endif ()
    endforeach ()
    set(${list_var} "${${list_var}}" PARENT_SCOPE)
endfunction()

file(GLOB VIOINPUT_TEST_SOURCES CONFIGURE_DEPENDS "${CMAKE_CURRENT_LIST_DIR}/*_test.c")

foreach(test_src IN LISTS VIOINPUT_TEST_SOURCES)
    get_filename_component(test_name "${test_src}" NAME_WE)

    # Convention: if ../src/<name>.c exists (where <name> is the test filename
    # without the "_test" suffix), link it in automatically.
    set(module "${test_name}")
    string(REGEX REPLACE "_test$" "" module "${module}")

    set(sources "${test_src}")

    if (EXISTS "${VIOINPUT_SRC_DIR}/${module}.c")
        vioinput_list_append_unique(sources "${VIOINPUT_SRC_DIR}/${module}.c")
    endif ()

    # Optional: allow tests to declare extra translation units to link.
    # Syntax (single line anywhere in the file):
    #   // TEST_DEPS: foo.c bar.c
    # or:
    #   // TEST_DEPS: foo bar   (".c" is implied)
    file(STRINGS "${test_src}" deps_lines REGEX "^//[ \t]*TEST_DEPS:[ \t]*")
    if (deps_lines)
        list(GET deps_lines 0 deps_line)
        string(REGEX REPLACE "^//[ \t]*TEST_DEPS:[ \t]*" "" deps_str "${deps_line}")
        string(STRIP "${deps_str}" deps_str)
        if (NOT deps_str STREQUAL "")
            string(REGEX REPLACE "[ \t]+" ";" deps_list "${deps_str}")
            foreach(dep IN LISTS deps_list)
                set(dep_file "${dep}")
                if (NOT dep_file MATCHES "\\.c$")
                    set(dep_file "${dep_file}.c")
                endif ()
                set(dep_path "${VIOINPUT_SRC_DIR}/${dep_file}")
                if (NOT EXISTS "${dep_path}")
                    message(FATAL_ERROR "virtio-input test '${test_name}' missing dependency: ${dep_path}")
                endif ()
                vioinput_list_append_unique(sources "${dep_path}")
            endforeach ()
        endif ()
    endif ()

    add_executable(${test_name} ${sources})
    set_target_properties(${test_name} PROPERTIES
        C_STANDARD 11
        C_STANDARD_REQUIRED YES
        C_EXTENSIONS NO
    )
    target_compile_options(${test_name} PRIVATE ${VIOINPUT_TEST_CFLAGS})
    add_test(NAME ${test_name} COMMAND ${test_name})
endforeach ()

