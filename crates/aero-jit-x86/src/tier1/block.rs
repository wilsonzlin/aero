use crate::Tier1Bus;
use aero_x86::tier1::{decode_one_mode, DecodedInst, InstKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLimits {
    pub max_insts: usize,
    pub max_bytes: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_insts: 64,
            max_bytes: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockEndKind {
    Jmp,
    Jcc,
    Call,
    Ret,
    ExitToInterpreter { next_rip: u64 },
    Limit { next_rip: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub entry_rip: u64,
    pub bitness: u32,
    pub insts: Vec<DecodedInst>,
    pub end_kind: BlockEndKind,
}

/// Decode a basic block starting at `entry_rip`.
///
/// Decoding stops when one of the following conditions is met:
/// - a control-flow instruction is decoded (branch/call/ret)
/// - an unsupported/invalid instruction is decoded (must exit to interpreter)
/// - `limits` are exceeded
#[must_use]
pub fn discover_block<B: Tier1Bus>(bus: &B, entry_rip: u64, limits: BlockLimits) -> BasicBlock {
    discover_block_mode(bus, entry_rip, limits, 64)
}

/// Decode a basic block starting at `entry_rip` using the requested x86 bitness.
///
/// This is a thin wrapper around the Tier1 minimal decoder (`aero_x86::tier1`) that allows
/// front-ends/tests to run 16/32-bit guest payloads without mis-decoding `0x40..=0x4F` as REX.
#[must_use]
#[track_caller]
pub fn discover_block_mode<B: Tier1Bus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
    bitness: u32,
) -> BasicBlock {
    let mut insts = Vec::new();
    let ip_mask = match bitness {
        32 => 0xffff_ffff,
        64 => u64::MAX,
        // Tier-1 decoder only partially models 16-bit mode (eg. some 16-bit ModRM addressing
        // forms), but applying the 16-bit IP mask here keeps instruction fetch consistent with
        // the architectural IP width if callers experiment with `bitness=16`.
        16 => 0xffff,
        other => panic!("invalid x86 bitness {other}"),
    };
    let entry_rip = entry_rip & ip_mask;
    let mut rip = entry_rip;
    let mut total_bytes = 0usize;

    loop {
        if insts.len() >= limits.max_insts || total_bytes >= limits.max_bytes {
            return BasicBlock {
                entry_rip,
                bitness,
                insts,
                end_kind: BlockEndKind::Limit { next_rip: rip },
            };
        }

        // Instruction fetch must respect architectural IP width for 16/32-bit guests.
        //
        // `Tier1Bus` helpers use `u64` wrapping arithmetic; for 16/32-bit modes we instead mask
        // each byte address so instructions that straddle the architectural wrap boundary (e.g.
        // EIP=0xFFFF_FFFF) decode consistently with the guest IP size.
        //
        // When the full 15-byte decode window is contiguous within the masked IP space, we can
        // use `Tier1Bus::fetch_window()` directly (avoids per-byte masking and heap allocation).
        let bytes = if ip_mask == u64::MAX || rip <= ip_mask - 14 {
            bus.fetch_window(rip)
        } else {
            let mut buf = [0u8; 15];
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = bus.read_u8(rip.wrapping_add(i as u64) & ip_mask);
            }
            buf
        };
        let inst = decode_one_mode(rip, &bytes, bitness);
        total_bytes += inst.len as usize;
        rip = inst.next_rip() & ip_mask;

        let end_kind = match inst.kind {
            InstKind::JmpRel { .. } => Some(BlockEndKind::Jmp),
            InstKind::JccRel { .. } => Some(BlockEndKind::Jcc),
            InstKind::CallRel { .. } => Some(BlockEndKind::Call),
            InstKind::Ret => Some(BlockEndKind::Ret),
            InstKind::Invalid => Some(BlockEndKind::ExitToInterpreter { next_rip: inst.rip }),
            _ => None,
        };

        insts.push(inst);

        if let Some(kind) = end_kind {
            return BasicBlock {
                entry_rip,
                bitness,
                insts,
                end_kind: kind,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::capture_panic_location;

    #[derive(Default)]
    struct DummyBus;

    impl Tier1Bus for DummyBus {
        fn read_u8(&self, _addr: u64) -> u8 {
            0
        }

        fn write_u8(&mut self, _addr: u64, _value: u8) {}
    }

    #[test]
    fn discover_block_mode_panics_at_call_site_on_invalid_bitness() {
        let bus = DummyBus;

        let expected_file = file!();
        let expected_line = line!() + 2;
        let (file, line) = capture_panic_location(|| {
            let _ = discover_block_mode(&bus, 0, BlockLimits::default(), 0);
        });
        assert_eq!(file, expected_file);
        assert_eq!(line, expected_line);
    }
}
