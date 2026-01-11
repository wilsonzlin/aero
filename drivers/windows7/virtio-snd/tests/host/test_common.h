/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/*
 * Keep assertions active in all build configurations.
 *
 * These host tests may be built under a CMake Release configuration, which
 * defines NDEBUG and would normally compile out assert() checks. Use explicit
 * aborting assertions instead.
 */

static inline void virtiosnd_test_fail(const char* file, int line, const char* msg)
{
    fprintf(stderr, "TEST FAIL at %s:%d: %s\n", file, line, msg);
    abort();
}

#define TEST_ASSERT(expr)                                                                                                 \
    do {                                                                                                                  \
        if (!(expr)) {                                                                                                    \
            virtiosnd_test_fail(__FILE__, __LINE__, #expr);                                                               \
        }                                                                                                                 \
    } while (0)

#define TEST_ASSERT_EQ_U32(a, b)                                                                                           \
    do {                                                                                                                  \
        uint32_t _va = (uint32_t)(a);                                                                                     \
        uint32_t _vb = (uint32_t)(b);                                                                                     \
        if (_va != _vb) {                                                                                                 \
            char _msg[256];                                                                                               \
            snprintf(_msg, sizeof(_msg), "%s == %s (0x%08" PRIx32 " vs 0x%08" PRIx32 ")", #a, #b, _va, _vb);             \
            virtiosnd_test_fail(__FILE__, __LINE__, _msg);                                                                \
        }                                                                                                                 \
    } while (0)

#define TEST_ASSERT_EQ_U64(a, b)                                                                                           \
    do {                                                                                                                  \
        uint64_t _va = (uint64_t)(a);                                                                                     \
        uint64_t _vb = (uint64_t)(b);                                                                                     \
        if (_va != _vb) {                                                                                                 \
            char _msg[256];                                                                                               \
            snprintf(_msg, sizeof(_msg), "%s == %s (0x%016" PRIx64 " vs 0x%016" PRIx64 ")", #a, #b, _va, _vb);           \
            virtiosnd_test_fail(__FILE__, __LINE__, _msg);                                                                \
        }                                                                                                                 \
    } while (0)

#define TEST_ASSERT_MEMEQ(a, b, n)                                                                                         \
    do {                                                                                                                  \
        if (memcmp((a), (b), (n)) != 0) {                                                                                 \
            char _msg[256];                                                                                               \
            snprintf(_msg, sizeof(_msg), "memcmp(%s,%s,%s)==0", #a, #b, #n);                                              \
            virtiosnd_test_fail(__FILE__, __LINE__, _msg);                                                                \
        }                                                                                                                 \
    } while (0)

