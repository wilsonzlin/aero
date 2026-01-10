use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aero_http_range::{parse_range_header, resolve_ranges};

const FILE_SIZE: u64 = 5_368_709_120; // 5 GiB
const HIGH_OFFSET: u64 = 4_294_967_296 + 123; // 2^32 + 123

const SENTINEL_HIGH: &[u8] = b"AERO_RANGE_4GB";
const SENTINEL_END: &[u8] = b"AERO_RANGE_END";

struct DiskImageServerHandle {
    port: u16,
    join: JoinHandle<()>,
}

impl DiskImageServerHandle {
    fn join(self) {
        self.join.join().expect("server thread panicked");
    }
}

struct TempFile {
    path: PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn http_range_supports_offsets_beyond_4gb_and_suffix_ranges() {
    let temp = create_sparse_test_image();

    // One server instance handles both requests to avoid repeated setup cost.
    let server = spawn_disk_image_server(temp.path.clone(), 2);

    // Explicit range starting beyond 2^32.
    let high_len = SENTINEL_HIGH.len() as u64;
    let high_end = HIGH_OFFSET + high_len - 1;
    let (status, headers, body) = http_get_range(server.port, &format!("bytes={HIGH_OFFSET}-{high_end}"));
    assert_eq!(status, 206);
    assert_eq!(body, SENTINEL_HIGH);

    let content_range = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
        .map(|(_, v)| v.as_str())
        .expect("missing Content-Range");
    assert_eq!(
        content_range,
        format!("bytes {HIGH_OFFSET}-{high_end}/{FILE_SIZE}")
    );

    // Suffix range on a file > 4 GiB.
    let end_len = SENTINEL_END.len() as u64;
    let (status, headers, body) = http_get_range(server.port, &format!("bytes=-{end_len}"));
    assert_eq!(status, 206);
    assert_eq!(body, SENTINEL_END);

    let start = FILE_SIZE - end_len;
    let end = FILE_SIZE - 1;
    let content_range = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
        .map(|(_, v)| v.as_str())
        .expect("missing Content-Range");
    assert_eq!(content_range, format!("bytes {start}-{end}/{FILE_SIZE}"));

    server.join();
}

fn create_sparse_test_image() -> TempFile {
    let mut path = std::env::temp_dir();
    let unique = format!(
        "aero_range_large_offsets_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    );
    path.push(unique);

    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create temp image");

    file.set_len(FILE_SIZE).expect("set_len");

    // Write a sentinel beyond 2^32 to catch 32-bit truncation bugs.
    file.seek(SeekFrom::Start(HIGH_OFFSET))
        .expect("seek high offset");
    file.write_all(SENTINEL_HIGH).expect("write sentinel");

    // Write another sentinel at the end for suffix-range requests.
    let end_offset = FILE_SIZE - SENTINEL_END.len() as u64;
    file.seek(SeekFrom::Start(end_offset))
        .expect("seek end offset");
    file.write_all(SENTINEL_END).expect("write end sentinel");
    file.flush().expect("flush");

    TempFile { path }
}

fn http_get_range(port: u16, range: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    stream
        .write_all(
            format!(
                "GET /disk.img HTTP/1.1\r\nHost: localhost\r\nRange: {range}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("write request");

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read response");

    let header_end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("missing header terminator");
    let (head, body) = resp.split_at(header_end + 4);
    let head_str = String::from_utf8_lossy(head);

    let mut lines = head_str.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse::<u16>()
        .expect("parse status");

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    (status, headers, body.to_vec())
}

fn spawn_disk_image_server(file_path: PathBuf, expected_requests: usize) -> DiskImageServerHandle {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    listener
        .set_nonblocking(true)
        .expect("set non-blocking");

    let join = thread::spawn(move || {
        for request_idx in 0..expected_requests {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (stream, _) = loop {
                match listener.accept() {
                    Ok(v) => break v,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            panic!("timeout waiting for request {request_idx}");
                        }
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(err) => panic!("accept failed: {err}"),
                }
            };
            handle_connection(stream, &file_path).expect("handle request");
        }
    });

    DiskImageServerHandle { port, join }
}

fn handle_connection(mut stream: TcpStream, file_path: &PathBuf) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;

    let mut req = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        req.extend_from_slice(&buf[..n]);
        if req.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if req.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    }

    let req_str = std::str::from_utf8(&req)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid utf8"))?;
    let range = req_str
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("range") {
                Some(value.trim())
            } else {
                None
            }
        })
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Range"))?;

    let size = std::fs::metadata(file_path)?.len();
    let specs =
        parse_range_header(range).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Range header"))?;
    let resolved =
        resolve_ranges(&specs, size, true).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "unsatisfiable Range"))?;
    if resolved.len() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "multi-range not supported in test server",
        ));
    }
    let r = resolved[0];
    let len = r.len();
    if len > usize::MAX as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "range too large",
        ));
    }

    let mut file = File::open(file_path)?;
    file.seek(SeekFrom::Start(r.start))?;
    let mut body = vec![0u8; len as usize];
    file.read_exact(&mut body)?;

    let mut headers = String::new();
    headers.push_str("HTTP/1.1 206 Partial Content\r\n");
    headers.push_str("Accept-Ranges: bytes\r\n");
    headers.push_str("Content-Type: application/octet-stream\r\n");
    headers.push_str(&format!("Content-Length: {len}\r\n"));
    headers.push_str(&format!(
        "Content-Range: bytes {}-{}/{}\r\n",
        r.start, r.end, size
    ));
    headers.push_str("Connection: close\r\n\r\n");

    stream.write_all(headers.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;

    Ok(())
}
