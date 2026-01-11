#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"

typedef struct _VIRTIOSND_TX {
    VIRTIOSND_QUEUE* TxQ;
} VIRTIOSND_TX, *PVIRTIOSND_TX;

