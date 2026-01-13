#![no_main]

use libfuzzer_sys::fuzz_target;

use aero_net_stack::{NetworkStack, StackConfig};

const MAX_FRAME_LEN: usize = 4096;

fuzz_target!(|data: &[u8]| {
    // Avoid pathological allocations/runtime from extremely large "frames".
    let frame = &data[..data.len().min(MAX_FRAME_LEN)];

    // Pure stack logic only; does not open sockets.
    let mut stack = NetworkStack::new(StackConfig::default());

    // Deterministic "clock" (do not use wall time).
    let now_ms = 0;

    let actions = stack.process_outbound_ethernet(frame, now_ms);

    // Drain actions so any produced buffers are promptly dropped.
    for action in actions {
        match action {
            aero_net_stack::Action::EmitFrame(buf) => {
                // Touch the buffer so it isn't optimized away.
                let _ = buf.len();
            }
            aero_net_stack::Action::TcpProxyConnect { .. } => {}
            aero_net_stack::Action::TcpProxySend { .. } => {}
            aero_net_stack::Action::TcpProxyClose { .. } => {}
            aero_net_stack::Action::UdpProxySend { .. } => {}
            aero_net_stack::Action::DnsResolve { .. } => {}
        }
    }
});

