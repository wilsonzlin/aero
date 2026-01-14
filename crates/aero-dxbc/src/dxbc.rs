use crate::ctab::{parse_ctab_chunk, ConstantTable};
use crate::error::DxbcError;
use crate::fourcc::FourCC;
use crate::rdef::{parse_rdef_chunk_for_fourcc, RdefChunk};
use crate::signature::{parse_signature_chunk_for_fourcc, SignatureChunk};
use core::fmt;

const DXBC_MAGIC: FourCC = FourCC(*b"DXBC");
const DXBC_HEADER_LEN: usize = 4 + 16 + 4 + 4 + 4; // magic + checksum + reserved + total_size + chunk_count
                                                   // Hard cap on chunk count to avoid O(n) parsing work and pathological offset tables on hostile input.
                                                   //
                                                   // Real-world DXBC containers contain a small handful of chunks (single digits). This value is
                                                   // intentionally generous while still preventing multi-megabyte offset tables and extremely large
                                                   // validation loops.
const MAX_DXBC_CHUNK_COUNT: u32 = 4096;

/// The fixed header of a `DXBC` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcHeader {
    /// Must be [`FourCC(*b"DXBC")`].
    pub magic: FourCC,
    /// The checksum stored in the container header (MD5).
    pub checksum: [u8; 16],
    /// Declared total size, in bytes, of this `DXBC` container.
    pub total_size: u32,
    /// Number of chunk offsets following the header.
    pub chunk_count: u32,
}

/// A single chunk within a `DXBC` container.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct DxbcChunk<'a> {
    /// The chunk identifier (e.g. `SHDR`, `SHEX`, `RDEF`).
    pub fourcc: FourCC,
    /// Raw chunk payload bytes.
    pub data: &'a [u8],
}

impl fmt::Debug for DxbcChunk<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DxbcChunk")
            .field("fourcc", &self.fourcc)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// A parsed `DXBC` container.
///
/// Parsing is strict about bounds: every offset and size is validated to ensure
/// it stays within the container's declared `total_size`.
#[derive(Debug, Clone)]
pub struct DxbcFile<'a> {
    bytes: &'a [u8],
    header: DxbcHeader,
    chunk_offsets: &'a [u8],
}

impl<'a> DxbcFile<'a> {
    /// Parses a `DXBC` container from `bytes`.
    ///
    /// The input is treated as **untrusted**: this function validates all
    /// offsets/sizes and never panics on malformed data.
    pub fn parse(bytes: &'a [u8]) -> Result<DxbcFile<'a>, DxbcError> {
        if bytes.len() < DXBC_HEADER_LEN {
            return Err(DxbcError::malformed_header(format!(
                "need at least {DXBC_HEADER_LEN} bytes, got {}",
                bytes.len()
            )));
        }

        let magic = read_fourcc(bytes, 0).map_err(|e| {
            DxbcError::malformed_header(format!("failed to read magic: {}", e.context()))
        })?;
        if magic != DXBC_MAGIC {
            return Err(DxbcError::malformed_header(format!(
                "bad magic {:?}, expected {:?}",
                magic, DXBC_MAGIC
            )));
        }

        let checksum = read_array_16(bytes, 4).map_err(|e| {
            DxbcError::malformed_header(format!("failed to read checksum: {}", e.context()))
        })?;

        // The 4 bytes after the checksum are currently unused for our purposes.
        let total_size = read_u32_le(bytes, 24).map_err(|e| {
            DxbcError::malformed_header(format!("failed to read total_size: {}", e.context()))
        })?;
        let chunk_count = read_u32_le(bytes, 28).map_err(|e| {
            DxbcError::malformed_header(format!("failed to read chunk_count: {}", e.context()))
        })?;
        if chunk_count > MAX_DXBC_CHUNK_COUNT {
            return Err(DxbcError::malformed_offsets(format!(
                "chunk_count {chunk_count} exceeds maximum {MAX_DXBC_CHUNK_COUNT}"
            )));
        }

        if total_size < DXBC_HEADER_LEN as u32 {
            return Err(DxbcError::malformed_header(format!(
                "total_size {total_size} is smaller than header size {DXBC_HEADER_LEN}"
            )));
        }

        let total_size_usize = total_size as usize;
        if total_size_usize > bytes.len() {
            return Err(DxbcError::out_of_bounds(format!(
                "total_size {total_size} exceeds buffer length {}",
                bytes.len()
            )));
        }

        let bytes = &bytes[..total_size_usize];

        let chunk_count_usize = chunk_count as usize;
        let offset_table_len = chunk_count_usize.checked_mul(4).ok_or_else(|| {
            DxbcError::malformed_offsets("chunk_count overflows offset table size")
        })?;

        let offset_table_end = DXBC_HEADER_LEN
            .checked_add(offset_table_len)
            .ok_or_else(|| {
                DxbcError::malformed_offsets("header size overflows when adding chunk offset table")
            })?;

        if offset_table_end > bytes.len() {
            return Err(DxbcError::malformed_offsets(format!(
                "chunk offset table ends at {offset_table_end}, but total_size is {}",
                bytes.len()
            )));
        }

        let chunk_offsets = &bytes[DXBC_HEADER_LEN..offset_table_end];
        for i in 0..chunk_count_usize {
            let offset_pos_in_table = i.checked_mul(4).ok_or_else(|| {
                DxbcError::malformed_offsets(format!(
                    "chunk offset {i} overflows offset table indexing"
                ))
            })?;
            let offset_pos_in_file = DXBC_HEADER_LEN
                .checked_add(offset_pos_in_table)
                .ok_or_else(|| {
                    DxbcError::malformed_offsets(
                        "offset table indexing overflows when added to header size",
                    )
                })?;

            let chunk_offset = read_u32_le(bytes, offset_pos_in_file).map_err(|e| {
                DxbcError::malformed_offsets(format!(
                    "failed to read chunk offset {i} at file offset {offset_pos_in_file}: {}",
                    e.context()
                ))
            })? as usize;

            if chunk_offset < offset_table_end {
                if chunk_offset < DXBC_HEADER_LEN {
                    return Err(DxbcError::malformed_offsets(format!(
                        "chunk {i} offset {chunk_offset} points into DXBC header (need >= {DXBC_HEADER_LEN})"
                    )));
                }
                return Err(DxbcError::malformed_offsets(format!(
                    "chunk {i} offset {chunk_offset} points into chunk offset table ({DXBC_HEADER_LEN}..{offset_table_end})"
                )));
            }

            let chunk_header_end = chunk_offset.checked_add(8).ok_or_else(|| {
                DxbcError::malformed_offsets(format!(
                    "chunk {i} offset {chunk_offset} overflows when reading header"
                ))
            })?;
            if chunk_header_end > bytes.len() {
                return Err(DxbcError::out_of_bounds(format!(
                    "chunk {i} header at {chunk_offset}..{chunk_header_end} is outside total_size {}",
                    bytes.len()
                )));
            }

            let fourcc = read_fourcc(bytes, chunk_offset).map_err(|e| {
                DxbcError::malformed_offsets(format!(
                    "failed to read chunk {i} fourcc at {chunk_offset}: {}",
                    e.context()
                ))
            })?;
            let chunk_size = read_u32_le(bytes, chunk_offset + 4).map_err(|e| {
                DxbcError::malformed_offsets(format!(
                    "failed to read chunk {i} size at {}: {}",
                    chunk_offset + 4,
                    e.context()
                ))
            })? as usize;

            let data_start = chunk_offset + 8;
            let data_end = data_start.checked_add(chunk_size).ok_or_else(|| {
                DxbcError::malformed_offsets(format!(
                    "chunk {i} size {chunk_size} overflows when computing data range"
                ))
            })?;
            if data_end > bytes.len() {
                return Err(DxbcError::out_of_bounds(format!(
                    "chunk {i} ({fourcc}) data at {data_start}..{data_end} is outside total_size {}",
                    bytes.len()
                )));
            }
        }

        let header = DxbcHeader {
            magic,
            checksum,
            total_size,
            chunk_count,
        };

        Ok(DxbcFile {
            bytes,
            header,
            chunk_offsets,
        })
    }

    /// Returns the parsed `DXBC` header.
    pub fn header(&self) -> &DxbcHeader {
        &self.header
    }

    /// Returns the raw bytes covered by the container's declared `total_size`.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Iterates over all chunks in file order.
    pub fn chunks(&self) -> impl Iterator<Item = DxbcChunk<'a>> + '_ {
        DxbcChunksIter {
            bytes: self.bytes,
            chunk_offsets: self.chunk_offsets,
            index: 0,
        }
    }

    /// Returns the first chunk matching `fourcc`, if any.
    pub fn get_chunk(&self, fourcc: FourCC) -> Option<DxbcChunk<'a>> {
        self.chunks().find(|chunk| chunk.fourcc == fourcc)
    }

    /// Iterates over all chunks matching `fourcc`, in file order.
    pub fn get_chunks(&self, fourcc: FourCC) -> impl Iterator<Item = DxbcChunk<'a>> + '_ {
        self.chunks().filter(move |chunk| chunk.fourcc == fourcc)
    }

    /// Returns and parses the first signature chunk matching `kind`, if any.
    ///
    /// Signature chunks include:
    /// - `ISGN`/`ISG1` (input)
    /// - `OSGN`/`OSG1` (output)
    /// - `PSGN`/`PSG1` (patch signature; used by some tessellation stages)
    /// - `PCSG`/`PCG1` (patch-constant signature)
    ///
    /// Some compilers emit variant IDs with a trailing `1` (`*SG1`), which this
    /// method also accepts.
    ///
    /// Behavior:
    /// - Tries all chunks with the exact requested `kind` in file order, and
    ///   returns the first one that parses successfully.
    /// - If none parse successfully, tries the known `*SGN`/`*SG1` (or `PCSG`/`PCG1`)
    ///   variant, in file order.
    /// - Returns `None` only if neither `kind` nor its variant are present in
    ///   the container.
    pub fn get_signature(&self, kind: FourCC) -> Option<Result<SignatureChunk, DxbcError>> {
        // Some toolchains emit signature chunk variant IDs with a trailing `1`
        // (e.g. `ISG1` instead of `ISGN`). Accept either spelling.
        let fallback_kind = match kind.0 {
            [b'I', b'S', b'G', b'N'] => Some(FourCC(*b"ISG1")),
            [b'O', b'S', b'G', b'N'] => Some(FourCC(*b"OSG1")),
            [b'P', b'S', b'G', b'N'] => Some(FourCC(*b"PSG1")),
            [b'P', b'C', b'S', b'G'] => Some(FourCC(*b"PCG1")),
            [b'I', b'S', b'G', b'1'] => Some(FourCC(*b"ISGN")),
            [b'O', b'S', b'G', b'1'] => Some(FourCC(*b"OSGN")),
            [b'P', b'S', b'G', b'1'] => Some(FourCC(*b"PSGN")),
            [b'P', b'C', b'G', b'1'] => Some(FourCC(*b"PCSG")),
            _ => None,
        };

        fn parse_first_matching<'a>(
            dxbc: &DxbcFile<'a>,
            kind: FourCC,
        ) -> Option<Result<SignatureChunk, DxbcError>> {
            let mut first_err = None;
            for chunk in dxbc.get_chunks(kind) {
                match parse_signature_chunk_for_fourcc(chunk.fourcc, chunk.data).map_err(|e| {
                    DxbcError::invalid_chunk(format!(
                        "{} signature chunk: {}",
                        chunk.fourcc,
                        e.context()
                    ))
                }) {
                    Ok(sig) => return Some(Ok(sig)),
                    Err(err) => {
                        if first_err.is_none() {
                            first_err = Some(err);
                        }
                    }
                }
            }
            first_err.map(Err)
        }

        let primary = parse_first_matching(self, kind);
        if matches!(primary, Some(Ok(_))) {
            return primary;
        }

        let Some(fallback_kind) = fallback_kind else {
            return primary;
        };

        match parse_first_matching(self, fallback_kind) {
            ok @ Some(Ok(_)) => ok,
            Some(Err(err)) if primary.is_none() => Some(Err(err)),
            _ => primary,
        }
    }

    /// Returns and parses the first `RDEF`-style resource definition chunk, if any.
    ///
    /// Most compilers emit the `RDEF` chunk ID, but some toolchains use alternate
    /// IDs (commonly `RD11`). This helper:
    /// - Tries `RDEF` chunks in file order, returning the first one that parses.
    /// - If none parse successfully (or none exist), tries the `RD11` variant.
    /// - Returns `None` only if neither `RDEF` nor `RD11` exist in the container.
    pub fn get_rdef(&self) -> Option<Result<RdefChunk, DxbcError>> {
        let primary = FourCC(*b"RDEF");
        let fallback = FourCC(*b"RD11");

        fn parse_first_matching<'a>(
            dxbc: &DxbcFile<'a>,
            kind: FourCC,
        ) -> Option<Result<RdefChunk, DxbcError>> {
            let mut first_err = None;
            for chunk in dxbc.get_chunks(kind) {
                match parse_rdef_chunk_for_fourcc(chunk.fourcc, chunk.data).map_err(|e| {
                    DxbcError::invalid_chunk(format!("{} chunk: {}", chunk.fourcc, e.context()))
                }) {
                    Ok(rdef) => return Some(Ok(rdef)),
                    Err(err) => {
                        if first_err.is_none() {
                            first_err = Some(err);
                        }
                    }
                }
            }
            first_err.map(Err)
        }

        let primary_res = parse_first_matching(self, primary);
        if matches!(primary_res, Some(Ok(_))) {
            return primary_res;
        }

        match parse_first_matching(self, fallback) {
            ok @ Some(Ok(_)) => ok,
            Some(Err(err)) if primary_res.is_none() => Some(Err(err)),
            _ => primary_res,
        }
    }

    /// Returns and parses the first `CTAB` constant table chunk, if any.
    pub fn get_ctab(&self) -> Option<Result<ConstantTable, DxbcError>> {
        let kind = FourCC(*b"CTAB");
        let mut first_err = None;
        for chunk in self.get_chunks(kind) {
            match parse_ctab_chunk(chunk.data).map_err(|e| {
                DxbcError::invalid_chunk(format!("{} chunk: {}", chunk.fourcc, e.context()))
            }) {
                Ok(ctab) => return Some(Ok(ctab)),
                Err(err) => {
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }
        first_err.map(Err)
    }

    /// Returns the first shader bytecode chunk (`SHEX` or `SHDR`) in file order.
    pub fn find_first_shader_chunk(&self) -> Option<DxbcChunk<'a>> {
        let shex = FourCC(*b"SHEX");
        let shdr = FourCC(*b"SHDR");
        self.chunks()
            .find(|chunk| chunk.fourcc == shex || chunk.fourcc == shdr)
    }

    /// Returns a human-readable summary of the container and its chunks.
    pub fn debug_summary(&self) -> String {
        let mut out = String::new();
        use core::fmt::Write as _;

        let _ = write!(
            &mut out,
            "{} total_size={} chunk_count={}",
            self.header.magic, self.header.total_size, self.header.chunk_count
        );

        for (idx, chunk) in self.chunks().enumerate() {
            let _ = write!(
                &mut out,
                "\n  [{idx:02}] {} {} bytes",
                chunk.fourcc,
                chunk.data.len()
            );
        }

        out
    }

    /// Computes the checksum (MD5) used by DXBC containers.
    ///
    /// This is provided behind the `md5` feature because most callers do not
    /// need checksum validation. Parsing does **not** fail if the checksum does
    /// not match; callers may opt-in to validation by comparing the computed
    /// value to [`DxbcHeader::checksum`].
    #[cfg(feature = "md5")]
    pub fn compute_md5_checksum(&self) -> [u8; 16] {
        let mut ctx = md5::Context::new();
        ctx.consume(&self.bytes[..4]);
        ctx.consume([0u8; 16]);
        ctx.consume(&self.bytes[20..]);
        ctx.compute().0
    }

    /// Returns `true` if the computed checksum matches the checksum stored in
    /// the container header.
    #[cfg(feature = "md5")]
    pub fn checksum_matches(&self) -> bool {
        self.compute_md5_checksum() == self.header.checksum
    }
}

struct DxbcChunksIter<'a> {
    bytes: &'a [u8],
    chunk_offsets: &'a [u8],
    index: usize,
}

impl<'a> Iterator for DxbcChunksIter<'a> {
    type Item = DxbcChunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.index.checked_mul(4)?;
        let end = start.checked_add(4)?;
        let offset_bytes = self.chunk_offsets.get(start..end)?;
        let chunk_offset = u32::from_le_bytes(offset_bytes.try_into().ok()?) as usize;

        let header_end = chunk_offset.checked_add(8)?;
        let header = self.bytes.get(chunk_offset..header_end)?;
        let fourcc = FourCC([header[0], header[1], header[2], header[3]]);
        let chunk_size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let data_start = chunk_offset.checked_add(8)?;
        let data_end = data_start.checked_add(chunk_size)?;
        let data = self.bytes.get(data_start..data_end)?;

        self.index = self.index.saturating_add(1);
        Some(DxbcChunk { fourcc, data })
    }
}

fn read_array_16(bytes: &[u8], offset: usize) -> Result<[u8; 16], DxbcError> {
    let end = offset.checked_add(16).ok_or_else(|| {
        DxbcError::malformed_header("offset overflows when reading 16-byte array")
    })?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::malformed_header(format!(
            "need 16 bytes at {offset}..{end}, but buffer length is {}",
            bytes.len()
        ))
    })?;
    let mut out = [0u8; 16];
    out.copy_from_slice(slice);
    Ok(out)
}

fn read_fourcc(bytes: &[u8], offset: usize) -> Result<FourCC, DxbcError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| DxbcError::malformed_header("offset overflows when reading fourcc"))?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::malformed_header(format!(
            "need 4 bytes at {offset}..{end}, but buffer length is {}",
            bytes.len()
        ))
    })?;
    let mut out = [0u8; 4];
    out.copy_from_slice(slice);
    Ok(FourCC(out))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, DxbcError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| DxbcError::malformed_header("offset overflows when reading u32"))?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::malformed_header(format!(
            "need 4 bytes at {offset}..{end}, but buffer length is {}",
            bytes.len()
        ))
    })?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}
