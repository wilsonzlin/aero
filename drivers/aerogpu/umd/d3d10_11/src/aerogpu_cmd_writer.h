#pragma once

// D3D10/11 shares the exact same command stream serialization as D3D9; reuse the
// writer implementation so future WDDM DMA-buffer plumbing can be shared.
#include "../../common/aerogpu_cmd_stream_writer.h"

namespace aerogpu {

using CmdWriter = CmdStreamWriter;
} // namespace aerogpu
