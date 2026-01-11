#pragma once

#include <ntddk.h>

#include "virtiosnd_queue.h"

typedef struct _VIRTIOSND_CONTROL {
    VIRTIOSND_QUEUE* ControlQ;
} VIRTIOSND_CONTROL, *PVIRTIOSND_CONTROL;

