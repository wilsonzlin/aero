#pragma once

#include <cstddef>
#include <cstdint>

namespace aerogpu {

struct Device;

// Ensures the current command stream has enough space for `bytes_needed` more
// bytes, acquiring/rebinding runtime-provided WDDM submit buffers when needed.
// Callers must hold `Device::mutex`.
bool ensure_cmd_space_locked(Device* dev, size_t bytes_needed);

// Submits the current command stream. Callers must hold `Device::mutex`.
uint64_t submit_locked(Device* dev, bool is_present = false);

} // namespace aerogpu
