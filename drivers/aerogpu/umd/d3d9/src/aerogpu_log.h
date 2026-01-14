#pragma once

#include <stdarg.h>

namespace aerogpu {

// Lightweight logging intended for early bring-up. In a real driver build this
// would likely be routed through ETW; for now we use OutputDebugStringA on
// Windows and stderr elsewhere.
void logf(const char* fmt, ...) noexcept;
void vlogf(const char* fmt, va_list args) noexcept;

} // namespace aerogpu
