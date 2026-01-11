use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncWriteExt, BufWriter},
    sync::Mutex,
};

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub dir: PathBuf,
    pub max_bytes: u64,
    pub max_files: usize,
}

#[derive(Clone)]
pub struct CaptureManager {
    inner: Option<Arc<CaptureManagerInner>>,
}

struct CaptureManagerInner {
    config: CaptureConfig,
    lock: Mutex<()>,
}

impl CaptureManager {
    pub async fn new(config: Option<CaptureConfig>) -> std::io::Result<Self> {
        let Some(config) = config else {
            return Ok(Self { inner: None });
        };

        if config.max_bytes == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "CAPTURE_MAX_BYTES must be > 0",
            ));
        }
        if config.max_files == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "CAPTURE_MAX_FILES must be > 0",
            ));
        }

        fs::create_dir_all(&config.dir).await?;
        Ok(Self {
            inner: Some(Arc::new(CaptureManagerInner {
                config,
                lock: Mutex::new(()),
            })),
        })
    }

    pub async fn open_connection_capture(
        &self,
        meta: ConnectionMeta<'_>,
    ) -> std::io::Result<Option<ConnectionCapture>> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok(None);
        };

        let _guard = inner.lock.lock().await;
        inner.enforce_limits_locked(None).await?;

        let ts_ms = system_time_ms(meta.started_at);
        let filename = format!("{ts_ms:013}-conn-{}.jsonl", meta.connection_id);
        let path = inner.config.dir.join(filename);

        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        let mut writer = BufWriter::new(file);

        let record = CaptureRecord::Meta {
            ts_ms,
            connection_id: meta.connection_id,
            client_ip: meta.client_ip,
            session_hash: meta.session_secret.map(hash_session),
            target: meta.target,
        };

        let line = serde_json::to_vec(&record)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
        writer.write_all(&line).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        Ok(Some(ConnectionCapture {
            path,
            writer: Arc::new(Mutex::new(writer)),
            manager: inner.clone(),
        }))
    }
}

#[derive(Clone)]
pub struct ConnectionCapture {
    path: PathBuf,
    writer: Arc<Mutex<BufWriter<fs::File>>>,
    manager: Arc<CaptureManagerInner>,
}

impl ConnectionCapture {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn record(&self, direction: Direction, data: &[u8]) -> std::io::Result<()> {
        let record = CaptureRecord::Chunk {
            ts_ms: now_ms(),
            direction,
            len: data.len(),
            data_b64: base64::engine::general_purpose::STANDARD.encode(data),
        };

        let line = serde_json::to_vec(&record)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;

        let mut writer = self.writer.lock().await;
        writer.write_all(&line).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    }

    pub async fn close(&self) -> std::io::Result<()> {
        {
            let mut writer = self.writer.lock().await;
            writer.flush().await?;
        }

        let _guard = self.manager.lock.lock().await;
        self.manager.enforce_limits_locked(Some(&self.path)).await
    }
}

#[derive(Copy, Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ClientToTarget,
    TargetToClient,
}

pub struct ConnectionMeta<'a> {
    pub connection_id: u64,
    pub started_at: SystemTime,
    pub client_ip: Option<IpAddr>,
    /// A per-client session secret. This value is never written to disk directly.
    pub session_secret: Option<&'a str>,
    pub target: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CaptureRecord<'a> {
    Meta {
        ts_ms: u64,
        connection_id: u64,
        client_ip: Option<IpAddr>,
        session_hash: Option<String>,
        target: &'a str,
    },
    Chunk {
        ts_ms: u64,
        direction: Direction,
        len: usize,
        data_b64: String,
    },
}

impl CaptureManagerInner {
    async fn enforce_limits_locked(&self, keep: Option<&Path>) -> std::io::Result<()> {
        let mut entries = Vec::new();
        let mut dir = match fs::read_dir(&self.config.dir).await {
            Ok(dir) => dir,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let meta = entry.metadata().await?;
            entries.push((path, meta.len()));
        }

        entries.sort_by(|(a, _), (b, _)| a.file_name().cmp(&b.file_name()));

        let mut total_bytes: u64 = entries.iter().map(|(_, bytes)| *bytes).sum();
        let mut total_files: usize = entries.len();

        while (total_files > self.config.max_files || total_bytes > self.config.max_bytes)
            && total_files > 1
        {
            let (path, bytes) = entries.remove(0);
            if keep.map_or(false, |keep| keep == path.as_path()) {
                entries.push((path, bytes));
                entries.sort_by(|(a, _), (b, _)| a.file_name().cmp(&b.file_name()));
                continue;
            }

            match fs::remove_file(&path).await {
                Ok(()) => {
                    total_bytes = total_bytes.saturating_sub(bytes);
                    total_files = total_files.saturating_sub(1);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }

        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn hash_session(session_secret: &str) -> String {
    let digest = Sha256::digest(session_secret.as_bytes());
    hex::encode(digest)
}
