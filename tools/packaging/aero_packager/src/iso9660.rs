use crate::FileToPackage;
use anyhow::{bail, Context, Result};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;

const SECTOR_SIZE: usize = 2048;
const SYSTEM_AREA_SECTORS: u32 = 16;

#[derive(Debug, Clone)]
pub struct IsoFileTree {
    pub paths: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct IsoFileEntry {
    pub path: String,
    pub extent_sector: u32,
    pub size: u32,
}

impl IsoFileTree {
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }
}

pub fn write_iso9660_joliet(
    out_path: &Path,
    volume_id: &str,
    source_date_epoch: i64,
    files: &[FileToPackage],
) -> Result<()> {
    if files.is_empty() {
        bail!("refusing to create an empty ISO");
    }

    let normalized_volume_id = normalize_volume_id(volume_id);

    let tree = build_tree(files)?;

    // Pre-compute identifiers for ISO and Joliet trees.
    let mut iso_ids = Identifiers::default();
    iso_ids.assign_iso_names(&tree)?;
    let joliet_ids = Identifiers::assign_joliet_names(&tree)?;

    // Directory sizes (always multiples of SECTOR_SIZE, because directory
    // records cannot cross sector boundaries).
    let iso_dir_sizes: Vec<u32> = (0..tree.dirs.len())
        .map(|dir_idx| compute_directory_size(dir_idx, &tree, &iso_ids, TreeKind::Iso9660))
        .collect();
    let joliet_dir_sizes: Vec<u32> = (0..tree.dirs.len())
        .map(|dir_idx| compute_directory_size(dir_idx, &tree, &joliet_ids, TreeKind::Joliet))
        .collect();

    let iso_path_table_len = compute_path_table_len(&tree, &iso_ids, TreeKind::Iso9660);
    let joliet_path_table_len = compute_path_table_len(&tree, &joliet_ids, TreeKind::Joliet);

    // Layout:
    // - System Area (16 sectors)
    // - PVD (1 sector)
    // - Joliet SVD (1 sector)
    // - Terminator (1 sector)
    // - Path tables (ISO LE, ISO BE, Joliet LE, Joliet BE)
    // - ISO directory extents (entire tree)
    // - Joliet directory extents (entire tree)
    // - File data extents (shared by both trees)

    let mut next_sector = SYSTEM_AREA_SECTORS + 3; // 3 volume descriptor sectors

    let iso_path_table_le_sector = next_sector;
    let iso_path_table_sectors = sectors_for_len(iso_path_table_len);
    next_sector += iso_path_table_sectors;

    let iso_path_table_be_sector = next_sector;
    next_sector += iso_path_table_sectors;

    let joliet_path_table_le_sector = next_sector;
    let joliet_path_table_sectors = sectors_for_len(joliet_path_table_len);
    next_sector += joliet_path_table_sectors;

    let joliet_path_table_be_sector = next_sector;
    next_sector += joliet_path_table_sectors;

    let mut iso_dir_sector = vec![0u32; tree.dirs.len()];
    for (idx, size) in iso_dir_sizes.iter().enumerate() {
        iso_dir_sector[idx] = next_sector;
        next_sector += sectors_for_len(*size);
    }

    let mut joliet_dir_sector = vec![0u32; tree.dirs.len()];
    for (idx, size) in joliet_dir_sizes.iter().enumerate() {
        joliet_dir_sector[idx] = next_sector;
        next_sector += sectors_for_len(*size);
    }

    let mut file_sector = vec![0u32; tree.files.len()];
    for (idx, file) in tree.files.iter().enumerate() {
        file_sector[idx] = next_sector;
        next_sector += sectors_for_len(file.bytes.len() as u32);
    }

    let volume_space_size = next_sector;

    // Build volume descriptors now that we know volume size and root extents.
    let iso_root_record = build_directory_record(
        iso_dir_sector[0],
        iso_dir_sizes[0],
        true,
        &[0u8],
        iso_timestamp_7(source_date_epoch),
    );
    let joliet_root_record = build_directory_record(
        joliet_dir_sector[0],
        joliet_dir_sizes[0],
        true,
        &[0u8],
        iso_timestamp_7(source_date_epoch),
    );

    let pvd = build_volume_descriptor(
        1,
        &normalized_volume_id,
        volume_space_size,
        iso_path_table_len,
        iso_path_table_le_sector,
        iso_path_table_be_sector,
        &iso_root_record,
        source_date_epoch,
        None,
    );
    let svd = build_volume_descriptor(
        2,
        &normalized_volume_id,
        volume_space_size,
        joliet_path_table_len,
        joliet_path_table_le_sector,
        joliet_path_table_be_sector,
        &joliet_root_record,
        source_date_epoch,
        Some(JolietLevel::Level3),
    );
    let vdst = build_terminator_descriptor();

    let iso_path_table_le = build_path_table(
        &tree,
        &iso_ids,
        TreeKind::Iso9660,
        Endian::Little,
        &iso_dir_sector,
    );
    let iso_path_table_be = build_path_table(
        &tree,
        &iso_ids,
        TreeKind::Iso9660,
        Endian::Big,
        &iso_dir_sector,
    );
    let joliet_path_table_le = build_path_table(
        &tree,
        &joliet_ids,
        TreeKind::Joliet,
        Endian::Little,
        &joliet_dir_sector,
    );
    let joliet_path_table_be = build_path_table(
        &tree,
        &joliet_ids,
        TreeKind::Joliet,
        Endian::Big,
        &joliet_dir_sector,
    );

    let mut iso_dirs_data = Vec::with_capacity(tree.dirs.len());
    for dir_idx in 0..tree.dirs.len() {
        iso_dirs_data.push(build_directory_extent(
            dir_idx,
            &tree,
            &iso_ids,
            TreeKind::Iso9660,
            &iso_dir_sector,
            &iso_dir_sizes,
            &file_sector,
            source_date_epoch,
        )?);
    }

    let mut joliet_dirs_data = Vec::with_capacity(tree.dirs.len());
    for dir_idx in 0..tree.dirs.len() {
        joliet_dirs_data.push(build_directory_extent(
            dir_idx,
            &tree,
            &joliet_ids,
            TreeKind::Joliet,
            &joliet_dir_sector,
            &joliet_dir_sizes,
            &file_sector,
            source_date_epoch,
        )?);
    }

    let mut out =
        File::create(out_path).with_context(|| format!("create iso {}", out_path.display()))?;

    // System area.
    write_zeros(&mut out, SYSTEM_AREA_SECTORS as usize * SECTOR_SIZE)?;

    out.write_all(&pvd)?;
    out.write_all(&svd)?;
    out.write_all(&vdst)?;

    write_padded(&mut out, &iso_path_table_le, iso_path_table_sectors)?;
    write_padded(&mut out, &iso_path_table_be, iso_path_table_sectors)?;
    write_padded(&mut out, &joliet_path_table_le, joliet_path_table_sectors)?;
    write_padded(&mut out, &joliet_path_table_be, joliet_path_table_sectors)?;

    for dir_data in iso_dirs_data {
        out.write_all(&dir_data)?;
    }
    for dir_data in joliet_dirs_data {
        out.write_all(&dir_data)?;
    }

    for f in &tree.files {
        out.write_all(&f.bytes)?;
        let padding = pad_to_sector(f.bytes.len());
        if padding != 0 {
            write_zeros(&mut out, padding)?;
        }
    }

    let expected_len = volume_space_size as usize * SECTOR_SIZE;
    let actual_len = out.metadata()?.len() as usize;
    if actual_len != expected_len {
        bail!("internal error: ISO size mismatch (expected {expected_len}, got {actual_len})");
    }

    Ok(())
}

pub fn read_joliet_tree(iso_bytes: &[u8]) -> Result<IsoFileTree> {
    let svd_offset = find_joliet_svd(iso_bytes)?;
    let svd = &iso_bytes[svd_offset..svd_offset + SECTOR_SIZE];

    let root_record = &svd[156..190];
    let root = parse_directory_record(root_record).context("parse joliet root directory record")?;
    if !root.is_dir {
        bail!("joliet root directory record is not a directory");
    }

    let mut paths = BTreeSet::new();
    walk_dir_joliet(iso_bytes, "", root.extent_sector, root.size, &mut paths)?;
    Ok(IsoFileTree { paths })
}

fn find_joliet_svd(iso_bytes: &[u8]) -> Result<usize> {
    for sector in SYSTEM_AREA_SECTORS..SYSTEM_AREA_SECTORS + 64 {
        let off = sector as usize * SECTOR_SIZE;
        if off + SECTOR_SIZE > iso_bytes.len() {
            break;
        }
        let vd = &iso_bytes[off..off + SECTOR_SIZE];
        let vd_type = vd[0];
        if &vd[1..6] != b"CD001" {
            continue;
        }
        if vd_type == 255 {
            break;
        }
        if vd_type != 2 {
            continue;
        }
        if vd[88..91] == [0x25, 0x2F, 0x40]
            || vd[88..91] == [0x25, 0x2F, 0x43]
            || vd[88..91] == [0x25, 0x2F, 0x45]
        {
            return Ok(off);
        }
    }
    bail!("joliet supplementary volume descriptor not found")
}

fn walk_dir_joliet(
    iso_bytes: &[u8],
    dir_path: &str,
    extent_sector: u32,
    size: u32,
    out: &mut BTreeSet<String>,
) -> Result<()> {
    let start = extent_sector as usize * SECTOR_SIZE;
    let end = start + size as usize;
    if end > iso_bytes.len() {
        bail!("directory extent out of bounds");
    }
    let data = &iso_bytes[start..end];

    let mut pos = 0usize;
    while pos < data.len() {
        let len = data[pos] as usize;
        if len == 0 {
            pos = ((pos / SECTOR_SIZE) + 1) * SECTOR_SIZE;
            continue;
        }
        if pos + len > data.len() {
            break;
        }
        let record = &data[pos..pos + len];
        let parsed = parse_directory_record(record)?;
        pos += len;

        // Skip self/parent entries.
        if parsed.special {
            continue;
        }

        let mut name = parsed.name.clone();
        if let Some(stripped) = name.strip_suffix(";1") {
            name = stripped.to_string();
        }

        let child_path = if dir_path.is_empty() {
            name
        } else {
            format!("{dir_path}/{name}")
        };

        if parsed.is_dir {
            walk_dir_joliet(
                iso_bytes,
                &child_path,
                parsed.extent_sector,
                parsed.size,
                out,
            )?;
        } else {
            out.insert(child_path);
        }
    }

    Ok(())
}

#[derive(Debug)]
struct ParsedDirRecord {
    extent_sector: u32,
    size: u32,
    is_dir: bool,
    special: bool,
    name: String,
}

fn parse_directory_record(record: &[u8]) -> Result<ParsedDirRecord> {
    if record.len() < 34 {
        bail!("directory record too short");
    }
    let extent_sector = u32::from_le_bytes([record[2], record[3], record[4], record[5]]);
    let size = u32::from_le_bytes([record[10], record[11], record[12], record[13]]);
    let flags = record[25];
    let is_dir = (flags & 0x02) != 0;
    let id_len = record[32] as usize;
    let id = &record[33..33 + id_len];
    let special = id_len == 1 && (id[0] == 0x00 || id[0] == 0x01);
    let name = if special {
        String::new()
    } else {
        decode_ucs2be(id)
    };
    Ok(ParsedDirRecord {
        extent_sector,
        size,
        is_dir,
        special,
        name,
    })
}

fn decode_ucs2be(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16_lossy(&units)
}

#[derive(Debug, Clone)]
struct Tree {
    dirs: Vec<DirNode>,
    files: Vec<FileNode>,
}

#[derive(Debug, Clone)]
struct DirNode {
    path: String,
    parent: Option<usize>,
    children_dirs: Vec<usize>,
    children_files: Vec<usize>,
    name: String,
}

#[derive(Debug, Clone)]
struct FileNode {
    rel_path: String,
    name: String,
    bytes: Vec<u8>,
}

fn build_tree(files: &[FileToPackage]) -> Result<Tree> {
    let mut dir_paths = BTreeSet::new();
    dir_paths.insert(String::new()); // root

    let mut seen_files = BTreeSet::new();
    for f in files {
        if f.rel_path.starts_with('/') {
            bail!("ISO paths must be relative (got {})", f.rel_path);
        }
        if f.rel_path.contains('\\') {
            bail!("ISO paths must use '/' separators (got {})", f.rel_path);
        }
        if !seen_files.insert(f.rel_path.clone()) {
            bail!("duplicate file path in ISO: {}", f.rel_path);
        }
        let parts: Vec<&str> = f.rel_path.split('/').collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            bail!("invalid ISO path: {}", f.rel_path);
        }
        for i in 1..parts.len() {
            dir_paths.insert(parts[..i].join("/"));
        }
    }

    let mut dirs: Vec<String> = dir_paths.into_iter().collect();
    dirs.sort();

    let mut dir_index = BTreeMap::new();
    for (idx, path) in dirs.iter().enumerate() {
        dir_index.insert(path.clone(), idx);
    }

    let mut dir_nodes: Vec<DirNode> = dirs
        .iter()
        .map(|p| {
            let (parent, name) = if p.is_empty() {
                (None, String::new())
            } else if let Some((parent, name)) = p.rsplit_once('/') {
                (Some(parent.to_string()), name.to_string())
            } else {
                (Some(String::new()), p.clone())
            };
            DirNode {
                path: p.clone(),
                parent: parent.as_ref().map(|pp| *dir_index.get(pp).unwrap()),
                children_dirs: Vec::new(),
                children_files: Vec::new(),
                name,
            }
        })
        .collect();

    // Populate children directories.
    for idx in 1..dir_nodes.len() {
        let parent = dir_nodes[idx].parent.expect("non-root has parent");
        dir_nodes[parent].children_dirs.push(idx);
    }
    let dir_names: Vec<String> = dir_nodes.iter().map(|n| n.name.clone()).collect();
    for node in &mut dir_nodes {
        node.children_dirs
            .sort_by(|a, b| dir_names[*a].cmp(&dir_names[*b]));
    }

    let mut file_nodes = Vec::new();
    for f in files {
        let (parent_path, name) = if let Some((parent, name)) = f.rel_path.rsplit_once('/') {
            (parent.to_string(), name.to_string())
        } else {
            (String::new(), f.rel_path.clone())
        };
        let parent_dir = *dir_index.get(&parent_path).unwrap();
        let idx = file_nodes.len();
        file_nodes.push(FileNode {
            rel_path: f.rel_path.clone(),
            name,
            bytes: f.bytes.clone(),
        });
        dir_nodes[parent_dir].children_files.push(idx);
    }
    for node in &mut dir_nodes {
        node.children_files
            .sort_by(|a, b| file_nodes[*a].name.cmp(&file_nodes[*b].name));
    }

    Ok(Tree {
        dirs: dir_nodes,
        files: file_nodes,
    })
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum TreeKind {
    Iso9660,
    Joliet,
}

#[derive(Debug, Copy, Clone)]
enum Endian {
    Little,
    Big,
}

#[derive(Debug, Copy, Clone)]
enum JolietLevel {
    Level3,
}

#[derive(Debug, Default, Clone)]
struct Identifiers {
    // For dirs/files, identifier bytes as stored in directory records / path tables.
    iso_dir_id: Vec<Vec<u8>>,
    iso_file_id: Vec<Vec<u8>>,
    joliet_dir_id: Vec<Vec<u8>>,
    joliet_file_id: Vec<Vec<u8>>,
}

impl Identifiers {
    fn assign_joliet_names(tree: &Tree) -> Result<Self> {
        let mut ids = Identifiers::default();
        ids.joliet_dir_id = vec![Vec::new(); tree.dirs.len()];
        ids.joliet_file_id = vec![Vec::new(); tree.files.len()];

        ids.joliet_dir_id[0] = vec![0u8];
        for (idx, dir) in tree.dirs.iter().enumerate().skip(1) {
            ids.joliet_dir_id[idx] = encode_ucs2be(&dir.name);
        }

        for (idx, file) in tree.files.iter().enumerate() {
            ids.joliet_file_id[idx] = encode_ucs2be(&file.name);
        }

        Ok(ids)
    }

    fn assign_iso_names(&mut self, tree: &Tree) -> Result<()> {
        self.iso_dir_id = vec![Vec::new(); tree.dirs.len()];
        self.iso_file_id = vec![Vec::new(); tree.files.len()];
        self.iso_dir_id[0] = vec![0u8];

        for parent_idx in 0..tree.dirs.len() {
            let mut used = BTreeSet::<Vec<u8>>::new();

            for child_dir in tree.dirs[parent_idx].children_dirs.iter().copied() {
                let child = &tree.dirs[child_dir];
                let full_path = &child.path;
                let id = make_unique_iso_id(&child.name, true, full_path, &mut used);
                self.iso_dir_id[child_dir] = id;
            }

            for child_file in tree.dirs[parent_idx].children_files.iter().copied() {
                let child = &tree.files[child_file];
                let full_path = &child.rel_path;
                let id = make_unique_iso_id(&child.name, false, full_path, &mut used);
                self.iso_file_id[child_file] = id;
            }
        }

        Ok(())
    }

    fn dir_id<'a>(&'a self, kind: TreeKind, idx: usize) -> &'a [u8] {
        match kind {
            TreeKind::Iso9660 => &self.iso_dir_id[idx],
            TreeKind::Joliet => &self.joliet_dir_id[idx],
        }
    }

    fn file_id<'a>(&'a self, kind: TreeKind, idx: usize) -> &'a [u8] {
        match kind {
            TreeKind::Iso9660 => &self.iso_file_id[idx],
            TreeKind::Joliet => &self.joliet_file_id[idx],
        }
    }
}

fn make_unique_iso_id(
    name: &str,
    is_dir: bool,
    full_path: &str,
    used: &mut BTreeSet<Vec<u8>>,
) -> Vec<u8> {
    let mut candidate = sanitize_iso_name(name, is_dir);
    if !is_dir {
        candidate.push_str(";1");
    }

    let mut bytes = candidate.as_bytes().to_vec();
    if bytes.len() > 31 || used.contains(&bytes) {
        let hash8 = short_hash_hex_upper(full_path.as_bytes());
        let suffix = if is_dir {
            format!("_{}", hash8)
        } else {
            format!("_{};1", hash8)
        };

        let max_base_len = 31usize.saturating_sub(suffix.len());
        let mut base = sanitize_iso_name(name, is_dir);
        if base.len() > max_base_len {
            base.truncate(max_base_len);
        }
        let final_name = format!("{base}{suffix}");
        bytes = final_name.as_bytes().to_vec();
    }

    used.insert(bytes.clone());
    bytes
}

fn sanitize_iso_name(name: &str, is_dir: bool) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let upper = c.to_ascii_uppercase();
        let ok = match upper {
            'A'..='Z' | '0'..='9' | '_' => true,
            '.' if !is_dir => true,
            _ => false,
        };
        out.push(if ok { upper } else { '_' });
    }

    // Avoid empty identifiers.
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn short_hash_hex_upper(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(8);
    for b in &digest[..4] {
        out.push_str(&format!("{:02X}", b));
    }
    out
}

fn encode_ucs2be(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

fn compute_path_table_len(tree: &Tree, ids: &Identifiers, kind: TreeKind) -> u32 {
    let mut len = 0u32;
    for dir_idx in 0..tree.dirs.len() {
        let id_len = ids.dir_id(kind, dir_idx).len() as u32;
        // 8 + id + padding (if id_len odd)
        len += 8 + id_len + if id_len % 2 == 1 { 1 } else { 0 };
    }
    len
}

fn compute_directory_size(dir_idx: usize, tree: &Tree, ids: &Identifiers, kind: TreeKind) -> u32 {
    let mut record_lens = Vec::new();
    // '.' and '..'
    record_lens.push(dir_record_len(1));
    record_lens.push(dir_record_len(1));

    for child_dir in tree.dirs[dir_idx].children_dirs.iter().copied() {
        record_lens.push(dir_record_len(ids.dir_id(kind, child_dir).len() as u32));
    }
    for child_file in tree.dirs[dir_idx].children_files.iter().copied() {
        record_lens.push(dir_record_len(ids.file_id(kind, child_file).len() as u32));
    }

    pack_records_len(&record_lens)
}

fn dir_record_len(id_len: u32) -> u32 {
    // Directory record length = 33 + id_len + padding. Padding is present when
    // id_len is even (the fixed portion is 33 bytes, i.e. odd).
    33 + id_len + if id_len % 2 == 0 { 1 } else { 0 }
}

fn pack_records_len(record_lens: &[u32]) -> u32 {
    let mut pos = 0u32;
    for len in record_lens {
        let sector_off = pos % SECTOR_SIZE as u32;
        if sector_off + len > SECTOR_SIZE as u32 {
            pos = (pos / SECTOR_SIZE as u32 + 1) * SECTOR_SIZE as u32;
        }
        pos += len;
    }
    // Directory files are written with full-sector padding.
    ((pos + (SECTOR_SIZE as u32 - 1)) / SECTOR_SIZE as u32) * SECTOR_SIZE as u32
}

fn build_path_table(
    tree: &Tree,
    ids: &Identifiers,
    kind: TreeKind,
    endian: Endian,
    dir_sectors: &[u32],
) -> Vec<u8> {
    let mut out = Vec::new();
    for dir_idx in 0..tree.dirs.len() {
        let id = ids.dir_id(kind, dir_idx);
        out.push(id.len() as u8);
        out.push(0u8); // ext attr record length

        let extent = dir_sectors[dir_idx];
        match endian {
            Endian::Little => out.extend_from_slice(&extent.to_le_bytes()),
            Endian::Big => out.extend_from_slice(&extent.to_be_bytes()),
        }

        let parent_num: u16 = if dir_idx == 0 {
            1
        } else {
            tree.dirs[dir_idx].parent.unwrap() as u16 + 1
        };
        match endian {
            Endian::Little => out.extend_from_slice(&parent_num.to_le_bytes()),
            Endian::Big => out.extend_from_slice(&parent_num.to_be_bytes()),
        }

        out.extend_from_slice(id);
        if id.len() % 2 == 1 {
            out.push(0u8);
        }
    }
    out
}

fn build_directory_extent(
    dir_idx: usize,
    tree: &Tree,
    ids: &Identifiers,
    kind: TreeKind,
    dir_sectors: &[u32],
    dir_sizes: &[u32],
    file_sectors: &[u32],
    source_date_epoch: i64,
) -> Result<Vec<u8>> {
    let mut records = Vec::new();
    let ts = iso_timestamp_7(source_date_epoch);

    let self_sector = dir_sectors[dir_idx];
    let self_size = dir_sizes[dir_idx];
    let parent_idx = tree.dirs[dir_idx].parent.unwrap_or(0);
    let parent_sector = dir_sectors[parent_idx];
    let parent_size = dir_sizes[parent_idx];

    // '.' entry
    records.push(build_directory_record(
        self_sector,
        self_size,
        true,
        &[0u8],
        ts,
    ));
    // '..' entry
    records.push(build_directory_record(
        parent_sector,
        parent_size,
        true,
        &[1u8],
        ts,
    ));

    for child_dir in tree.dirs[dir_idx].children_dirs.iter().copied() {
        records.push(build_directory_record(
            dir_sectors[child_dir],
            dir_sizes[child_dir],
            true,
            ids.dir_id(kind, child_dir),
            ts,
        ));
    }

    for child_file in tree.dirs[dir_idx].children_files.iter().copied() {
        let file = &tree.files[child_file];
        records.push(build_directory_record(
            file_sectors[child_file],
            file.bytes.len() as u32,
            false,
            ids.file_id(kind, child_file),
            ts,
        ));
    }

    pack_directory_records(&records, self_size)
}

fn pack_directory_records(records: &[Vec<u8>], total_size: u32) -> Result<Vec<u8>> {
    let mut out = vec![0u8; total_size as usize];
    let mut pos = 0usize;
    for rec in records {
        let len = rec.len();
        let sector_off = pos % SECTOR_SIZE;
        if sector_off + len > SECTOR_SIZE {
            pos = (pos / SECTOR_SIZE + 1) * SECTOR_SIZE;
        }
        if pos + len > out.len() {
            bail!("directory extent overflow");
        }
        out[pos..pos + len].copy_from_slice(rec);
        pos += len;
    }
    Ok(out)
}

fn build_directory_record(
    extent_sector: u32,
    data_len: u32,
    is_dir: bool,
    identifier: &[u8],
    timestamp: [u8; 7],
) -> Vec<u8> {
    let id_len = identifier.len();
    let padding = if id_len % 2 == 0 { 1 } else { 0 };
    let record_len = 33 + id_len + padding;

    let mut out = vec![0u8; record_len];
    out[0] = record_len as u8;
    out[1] = 0u8; // ext attr record length
    out[2..6].copy_from_slice(&extent_sector.to_le_bytes());
    out[6..10].copy_from_slice(&extent_sector.to_be_bytes());
    out[10..14].copy_from_slice(&data_len.to_le_bytes());
    out[14..18].copy_from_slice(&data_len.to_be_bytes());
    out[18..25].copy_from_slice(&timestamp);
    out[25] = if is_dir { 0x02 } else { 0x00 };
    out[26] = 0u8; // file unit size
    out[27] = 0u8; // interleave gap size
    out[28..30].copy_from_slice(&1u16.to_le_bytes());
    out[30..32].copy_from_slice(&1u16.to_be_bytes());
    out[32] = id_len as u8;
    out[33..33 + id_len].copy_from_slice(identifier);
    // Padding/system use are already zero.
    out
}

fn build_volume_descriptor(
    vd_type: u8,
    volume_id: &str,
    volume_space_size: u32,
    path_table_size: u32,
    path_table_le_sector: u32,
    path_table_be_sector: u32,
    root_dir_record: &[u8],
    source_date_epoch: i64,
    joliet: Option<JolietLevel>,
) -> [u8; SECTOR_SIZE] {
    let mut out = [0u8; SECTOR_SIZE];
    out[0] = vd_type;
    out[1..6].copy_from_slice(b"CD001");
    out[6] = 1u8;
    // out[7] unused

    write_padded_ascii(&mut out[8..40], "AERO");
    write_padded_ascii(&mut out[40..72], volume_id);

    // volume space size (both-endian u32)
    out[80..84].copy_from_slice(&volume_space_size.to_le_bytes());
    out[84..88].copy_from_slice(&volume_space_size.to_be_bytes());

    if let Some(level) = joliet {
        let esc = match level {
            JolietLevel::Level3 => [0x25, 0x2F, 0x45],
        };
        out[88..91].copy_from_slice(&esc);
    }

    // volume set size, volume sequence number, logical block size (2048)
    write_both_endian_u16(&mut out[120..124], 1);
    write_both_endian_u16(&mut out[124..128], 1);
    write_both_endian_u16(&mut out[128..132], SECTOR_SIZE as u16);

    // path table size (both-endian u32)
    out[132..136].copy_from_slice(&path_table_size.to_le_bytes());
    out[136..140].copy_from_slice(&path_table_size.to_be_bytes());

    // path table locations
    out[140..144].copy_from_slice(&path_table_le_sector.to_le_bytes());
    // optional L path table: leave 0
    out[148..152].copy_from_slice(&path_table_be_sector.to_be_bytes());
    // optional M path table: leave 0

    // root directory record (34 bytes)
    out[156..190].copy_from_slice(&root_dir_record[..34]);

    let dt17 = iso_datetime_17(source_date_epoch);
    out[813..830].copy_from_slice(&dt17);
    out[830..847].copy_from_slice(&dt17);
    // Expiration/effective: keep as zeros except effective == creation for determinism.
    out[864..881].copy_from_slice(&dt17);

    out[881] = 1u8; // file structure version
    out
}

fn build_terminator_descriptor() -> [u8; SECTOR_SIZE] {
    let mut out = [0u8; SECTOR_SIZE];
    out[0] = 255u8;
    out[1..6].copy_from_slice(b"CD001");
    out[6] = 1u8;
    out
}

fn write_padded_ascii(dst: &mut [u8], s: &str) {
    dst.fill(b' ');
    let bytes = s.as_bytes();
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
}

fn write_both_endian_u16(dst: &mut [u8], value: u16) {
    dst[..2].copy_from_slice(&value.to_le_bytes());
    dst[2..4].copy_from_slice(&value.to_be_bytes());
}

fn normalize_volume_id(volume_id: &str) -> String {
    let mut out = String::new();
    for c in volume_id.chars() {
        let upper = c.to_ascii_uppercase();
        let ok = matches!(upper, 'A'..='Z' | '0'..='9' | '_' | ' ');
        out.push(if ok { upper } else { '_' });
    }
    if out.len() > 32 {
        out.truncate(32);
    }
    if out.is_empty() {
        out.push_str("AERO_GUEST_TOOLS");
        out.truncate(32);
    }
    out
}

fn sectors_for_len(len: u32) -> u32 {
    if len == 0 {
        0
    } else {
        (len + SECTOR_SIZE as u32 - 1) / SECTOR_SIZE as u32
    }
}

fn pad_to_sector(len: usize) -> usize {
    let rem = len % SECTOR_SIZE;
    if rem == 0 {
        0
    } else {
        SECTOR_SIZE - rem
    }
}

fn write_padded(out: &mut File, data: &[u8], sectors: u32) -> Result<()> {
    out.write_all(data)?;
    let padding_len = sectors as usize * SECTOR_SIZE - data.len();
    if padding_len > 0 {
        write_zeros(out, padding_len)?;
    }
    Ok(())
}

fn write_zeros(out: &mut File, len: usize) -> Result<()> {
    const ZERO: [u8; 4096] = [0u8; 4096];
    let mut remaining = len;
    while remaining > 0 {
        let chunk = remaining.min(ZERO.len());
        out.write_all(&ZERO[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn iso_timestamp_7(epoch: i64) -> [u8; 7] {
    let dt = time::OffsetDateTime::from_unix_timestamp(epoch)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    [
        (dt.year() - 1900) as u8,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
        dt.second() as u8,
        0u8, // GMT offset (15 min intervals)
    ]
}

fn iso_datetime_17(epoch: i64) -> [u8; 17] {
    let dt = time::OffsetDateTime::from_unix_timestamp(epoch)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let s = format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}00",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    );
    let mut out = [0u8; 17];
    out[..16].copy_from_slice(&s.as_bytes()[..16]);
    out[16] = 0u8;
    out
}
