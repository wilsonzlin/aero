#pragma once

#include <cstdint>

namespace aerogpu {

struct Device;

// Submits the current command stream. Callers must hold `Device::mutex`.
uint64_t submit_locked(Device* dev, bool is_present = false);

} // namespace aerogpu

