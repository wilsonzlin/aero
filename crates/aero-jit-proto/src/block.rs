use crate::x86::{DecodeError, DecodedInst, Decoder, InstKind};

/// A minimal interface for fetching guest code bytes.
///
/// The baseline JIT's block discovery pass scans linear bytes and decodes
/// sequentially; paging/MMU is modeled at a higher layer. Implementations are
/// expected to return a stable slice for the requested range.
pub trait CodeSource {
    fn fetch_code(&self, addr: u64, len: usize) -> Option<&[u8]>;
}

#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub start_rip: u64,
    pub insts: Vec<DecodedInst>,
    /// RIP immediately after the last instruction in the block.
    pub end_rip: u64,
    pub fallthrough: Option<u64>,
    pub branch_target: Option<u64>,
    /// Unique 4KiB pages that contain this block's instruction bytes.
    pub code_pages: Vec<u64>,
}

#[derive(Clone, Debug)]
pub enum BlockBuildError {
    FetchFailed(u64),
    Decode(DecodeError),
    UnsupportedTerminator,
    MaxBytesExceeded,
}

#[derive(Clone, Debug)]
pub struct BlockBuilder {
    decoder: Decoder,
    max_insts: usize,
    max_bytes: usize,
}

impl Default for BlockBuilder {
    fn default() -> Self {
        Self {
            decoder: Decoder::default(),
            max_insts: 64,
            max_bytes: 512,
        }
    }
}

impl BlockBuilder {
    pub fn new(max_insts: usize, max_bytes: usize) -> Self {
        Self {
            decoder: Decoder::default(),
            max_insts,
            max_bytes,
        }
    }

    pub fn build(
        &self,
        mem: &impl CodeSource,
        start_rip: u64,
    ) -> Result<BasicBlock, BlockBuildError> {
        let mut insts = Vec::new();
        let mut rip = start_rip;
        let mut total_bytes = 0usize;

        loop {
            if insts.len() >= self.max_insts {
                break;
            }
            if total_bytes >= self.max_bytes {
                return Err(BlockBuildError::MaxBytesExceeded);
            }

            // End blocks on syscall/interrupt instructions even if we don't
            // support lowering them yet (Tier-0 interpreter will handle).
            if let Some(bytes) = mem.fetch_code(rip, 2) {
                match bytes {
                    [0x0F, 0x05] => return Err(BlockBuildError::UnsupportedTerminator), // SYSCALL
                    [0xCC, _] | [0xCD, _] => return Err(BlockBuildError::UnsupportedTerminator), // INT3/INT imm8
                    _ => {}
                }
            }

            let bytes = mem
                .fetch_code(rip, 15)
                .ok_or(BlockBuildError::FetchFailed(rip))?;
            let inst = self
                .decoder
                .decode(bytes, rip)
                .map_err(BlockBuildError::Decode)?;

            total_bytes += inst.len as usize;
            rip = rip.wrapping_add(inst.len as u64);
            let is_control_flow = inst.kind.is_control_flow();

            insts.push(inst);

            if is_control_flow {
                break;
            }
        }

        let end_rip = rip;
        let mut fallthrough = Some(end_rip);
        let mut branch_target = None;

        if let Some(last) = insts.last() {
            match &last.kind {
                InstKind::Jmp { target } => {
                    fallthrough = None;
                    branch_target = Some(*target);
                }
                InstKind::Jcc {
                    target,
                    fallthrough: ft,
                    ..
                } => {
                    fallthrough = Some(*ft);
                    branch_target = Some(*target);
                }
                InstKind::Ret | InstKind::Hlt => {
                    fallthrough = None;
                    branch_target = None;
                }
                _ => {}
            }
        }

        let code_pages = compute_code_pages(&insts);

        Ok(BasicBlock {
            start_rip,
            insts,
            end_rip,
            fallthrough,
            branch_target,
            code_pages,
        })
    }
}

fn compute_code_pages(insts: &[DecodedInst]) -> Vec<u64> {
    let mut pages = Vec::<u64>::new();
    for inst in insts {
        let start = inst.rip;
        let end = inst.rip.wrapping_add(inst.len as u64).saturating_sub(1);
        let mut page = start >> 12;
        let end_page = end >> 12;
        while page <= end_page {
            pages.push(page);
            page += 1;
        }
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}
