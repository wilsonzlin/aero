#pragma once

#include "aerogpu_cmd_stream_writer.h"

namespace aerogpu {

// Backwards-compatible alias for older bring-up code that used `CmdWriter`.
// New code should prefer `CmdStreamWriter` / `SpanCmdStreamWriter` directly.
using CmdWriter = CmdStreamWriter;
} // namespace aerogpu
