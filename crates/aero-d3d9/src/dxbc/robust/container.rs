use super::byte_reader::ByteReader;
use super::{DxbcError, FourCc};

use super::chunks::DxbcChunk;

#[derive(Debug, Clone)]
pub struct DxbcContainer<'a> {
    pub total_size: u32,
    pub chunks: Vec<DxbcChunk<'a>>,
}

impl<'a> DxbcContainer<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DxbcError> {
        // Header: magic (4) + checksum (16) + unknown (4) + total_size (4) + chunk_count (4).
        const HEADER_BASE_SIZE: usize = 4 + 16 + 4 + 4 + 4;

        let mut r = ByteReader::new(bytes);

        let magic = FourCc::new(r.read_fourcc()?);
        if magic.as_bytes() != b"DXBC" {
            return Err(DxbcError::InvalidMagic { found: magic });
        }

        // 16-byte checksum; not validated here.
        let _checksum = r.read_bytes(16)?;
        let _unknown = r.read_u32_le()?;
        let total_size = r.read_u32_le()?;

        let total_size_usize = total_size as usize;
        if total_size_usize > bytes.len() {
            return Err(DxbcError::InvalidContainerSize {
                declared: total_size,
                actual: bytes.len(),
            });
        }
        if total_size_usize < HEADER_BASE_SIZE {
            return Err(DxbcError::InvalidContainerSizeTooSmall {
                declared: total_size,
                minimum: HEADER_BASE_SIZE,
            });
        }

        let chunk_count = r.read_u32_le()?;

        let offsets_bytes = (chunk_count as usize).checked_mul(4).ok_or(
            DxbcError::InvalidContainerSizeTooSmall {
                declared: total_size,
                minimum: HEADER_BASE_SIZE,
            },
        )?;

        let header_size = HEADER_BASE_SIZE.checked_add(offsets_bytes).ok_or(
            DxbcError::InvalidContainerSizeTooSmall {
                declared: total_size,
                minimum: HEADER_BASE_SIZE,
            },
        )?;

        if header_size > total_size_usize {
            return Err(DxbcError::InvalidContainerSizeTooSmall {
                declared: total_size,
                minimum: header_size,
            });
        }

        let mut chunk_offsets = Vec::with_capacity(chunk_count as usize);
        for _ in 0..chunk_count {
            chunk_offsets.push(r.read_u32_le()?);
        }

        let mut chunks = Vec::with_capacity(chunk_count as usize);
        for (chunk_index, &offset) in chunk_offsets.iter().enumerate() {
            if offset as usize >= total_size_usize {
                return Err(DxbcError::ChunkOffsetOutOfBounds {
                    chunk_index: chunk_index as u32,
                    offset,
                });
            }

            if (offset as usize).saturating_add(8) > total_size_usize {
                return Err(DxbcError::ChunkHeaderOutOfBounds {
                    chunk_index: chunk_index as u32,
                    offset,
                });
            }

            let mut cr = r.fork(offset as usize)?;
            let fourcc = FourCc::new(cr.read_fourcc()?);
            let size = cr.read_u32_le()?;

            let data_start = (offset as usize).saturating_add(8);
            let data_end =
                data_start
                    .checked_add(size as usize)
                    .ok_or(DxbcError::ChunkDataOutOfBounds {
                        chunk_index: chunk_index as u32,
                        fourcc,
                        offset,
                        size,
                        container_size: total_size_usize,
                    })?;

            if data_end > total_size_usize {
                return Err(DxbcError::ChunkDataOutOfBounds {
                    chunk_index: chunk_index as u32,
                    fourcc,
                    offset,
                    size,
                    container_size: total_size_usize,
                });
            }

            let data = &bytes[data_start..data_end];
            chunks.push(DxbcChunk {
                fourcc,
                offset,
                size,
                data,
            });
        }

        Ok(Self { total_size, chunks })
    }

    pub fn find_first(&self, fourcc: &FourCc) -> Option<&DxbcChunk<'a>> {
        self.chunks.iter().find(|c| &c.fourcc == fourcc)
    }
}
