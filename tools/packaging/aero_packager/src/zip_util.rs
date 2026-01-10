use crate::FileToPackage;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_deterministic_zip(
    path: &Path,
    source_date_epoch: i64,
    files: &[FileToPackage],
) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = zip::ZipWriter::new(file);

    let mtime = zip_datetime_from_epoch(source_date_epoch);

    let mut dirs: Vec<String> = collect_dirs(files).into_iter().collect();
    dirs.sort();

    for dir in dirs {
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(mtime)
            .unix_permissions(0o755);
        writer
            .add_directory(dir, options)
            .context("add directory entry")?;
    }

    for f in files {
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(9))
            .last_modified_time(mtime)
            .unix_permissions(0o644);

        writer
            .start_file(f.rel_path.clone(), options)
            .with_context(|| format!("start zip entry {}", f.rel_path))?;
        writer
            .write_all(&f.bytes)
            .with_context(|| format!("write zip entry {}", f.rel_path))?;
    }

    writer.finish().context("finish zip")?;
    Ok(())
}

fn collect_dirs(files: &[FileToPackage]) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    for f in files {
        let path = PathBuf::from(&f.rel_path);
        for ancestor in path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            let mut s = ancestor.to_string_lossy().replace('\\', "/");
            if !s.ends_with('/') {
                s.push('/');
            }
            dirs.insert(s);
        }
    }
    dirs
}

fn zip_datetime_from_epoch(epoch: i64) -> zip::DateTime {
    let dt = time::OffsetDateTime::from_unix_timestamp(epoch)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let mut year = dt.year();
    let mut month = dt.month() as u8;
    let mut day = dt.day() as u8;
    let mut hour = dt.hour() as u8;
    let mut minute = dt.minute() as u8;
    let mut second = dt.second() as u8;

    // DOS timestamp range is 1980-2107. Clamp so the produced zip is valid even
    // when SOURCE_DATE_EPOCH is 0 (1970).
    if year < 1980 {
        year = 1980;
        month = 1;
        day = 1;
        hour = 0;
        minute = 0;
        second = 0;
    } else if year > 2107 {
        year = 2107;
        month = 12;
        day = 31;
        hour = 23;
        minute = 59;
        second = 58; // ZIP has 2s resolution; be conservative.
    }

    zip::DateTime::from_date_and_time(year as u16, month, day, hour, minute, second)
        .unwrap_or_default()
}
