#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AEROGPU_DBGCTL_ESCAPE_ABI_VERSION 1u

#define AEROGPU_DBGCTL_U32_MAKE(a, b, c, d) \
  ((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) | ((uint32_t)(d) << 24))

#define AEROGPU_DBGCTL_ESCAPE_MAGIC AEROGPU_DBGCTL_U32_MAKE('A', 'G', 'D', 'B')

typedef enum AEROGPU_DBGCTL_OP {
  AEROGPU_DBGCTL_OP_QUERY_VERSION = 1,
  AEROGPU_DBGCTL_OP_QUERY_FENCE = 2,
  AEROGPU_DBGCTL_OP_DUMP_RING = 3,
  AEROGPU_DBGCTL_OP_SELFTEST = 4,
} AEROGPU_DBGCTL_OP;

#pragma pack(push, 1)

typedef struct AEROGPU_DBGCTL_HEADER {
  uint32_t magic;
  uint32_t abiVersion;
  uint32_t op;
  uint32_t reserved;
} AEROGPU_DBGCTL_HEADER;

typedef struct AEROGPU_DBGCTL_QUERY_VERSION {
  AEROGPU_DBGCTL_HEADER hdr;
  uint32_t deviceAbiMajor;
  uint32_t deviceAbiMinor;
  uint32_t kmdVersionMajor;
  uint32_t kmdVersionMinor;
  uint32_t umdVersionMajor;
  uint32_t umdVersionMinor;
  uint32_t reserved[10];
} AEROGPU_DBGCTL_QUERY_VERSION;

typedef struct AEROGPU_DBGCTL_QUERY_FENCE {
  AEROGPU_DBGCTL_HEADER hdr;
  uint64_t lastSubmittedFence;
  uint64_t lastCompletedFence;
  uint64_t reserved[4];
} AEROGPU_DBGCTL_QUERY_FENCE;

#define AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS 32u

typedef struct AEROGPU_DBGCTL_RING_DESC {
  uint64_t fence;
  uint64_t cmdGpuVa;
  uint32_t cmdBytes;
  uint32_t flags;
} AEROGPU_DBGCTL_RING_DESC;

typedef struct AEROGPU_DBGCTL_DUMP_RING {
  AEROGPU_DBGCTL_HEADER hdr;
  uint32_t ringId;
  uint32_t ringSizeBytes;
  uint32_t head;
  uint32_t tail;
  uint32_t descCount;
  uint32_t descCapacity;
  AEROGPU_DBGCTL_RING_DESC desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} AEROGPU_DBGCTL_DUMP_RING;

typedef struct AEROGPU_DBGCTL_SELFTEST {
  AEROGPU_DBGCTL_HEADER hdr;
  uint32_t timeoutMs;
  uint32_t passed;
  uint32_t errorCode;
  uint32_t reserved[13];
} AEROGPU_DBGCTL_SELFTEST;

#pragma pack(pop)

#ifdef __cplusplus
}
#endif

