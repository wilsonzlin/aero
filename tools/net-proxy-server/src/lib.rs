use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    sync::oneshot,
    task::JoinHandle,
};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tracing::{debug, info, warn};

#[derive(Debug)]
pub struct ProxyServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    active_connections: Arc<AtomicUsize>,
}

impl ProxyServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ProxyServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyServerOptions {
    pub bind_addr: SocketAddr,
}

impl Default for ProxyServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        }
    }
}

pub async fn start_proxy_server(opts: ProxyServerOptions) -> std::io::Result<ProxyServerHandle> {
    let listener = TcpListener::bind(opts.bind_addr).await?;
    let addr = listener.local_addr()?;

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let active_connections = Arc::new(AtomicUsize::new(0));
    let active_connections_task = active_connections.clone();

    let task = tokio::spawn(async move {
        info!(%addr, "net proxy server started");
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("net proxy server shutting down");
                    break;
                }
                accept = listener.accept() => {
                    let (tcp, peer) = match accept {
                        Ok(v) => v,
                        Err(err) => {
                            warn!(?err, "accept failed");
                            continue;
                        }
                    };
                    debug!(%peer, "proxy client connected");
                    active_connections_task.fetch_add(1, Ordering::SeqCst);
                    let active_connections_task = active_connections_task.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_ws_client(tcp).await {
                            warn!(?err, "proxy client error");
                        }
                        active_connections_task.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            }
        }
    });

    Ok(ProxyServerHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        active_connections,
    })
}

async fn handle_ws_client(stream: TcpStream) -> anyhow::Result<()> {
    let mut target: Option<(String, u16)> = None;
    let ws_stream: WebSocketStream<TcpStream> = tokio_tungstenite::accept_hdr_async(
        stream,
        |req: &tokio_tungstenite::tungstenite::http::Request<()>, resp| {
            let uri = req.uri();
            let path = uri.path();
            if path != "/tcp" {
                return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(404)
                    .body(Some("invalid path".to_string()))
                    .expect("builder"));
            }
            let query = uri.query().unwrap_or("");

            let mut host: Option<String> = None;
            let mut port: Option<u16> = None;
            for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
                match k.as_ref() {
                    "target" => {
                        if let Some(parsed) = parse_target(&v) {
                            target = Some(parsed);
                        }
                    }
                    "host" => {
                        host = Some(v.into_owned());
                    }
                    "port" => {
                        if let Ok(p) = v.parse::<u16>() {
                            port = Some(p);
                        }
                    }
                    _ => {}
                }
            }

            if target.is_none() {
                if let (Some(host), Some(port)) = (host, port) {
                    target = Some((normalize_host(&host), port));
                }
            }

            if target.is_none() {
                return Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(400)
                    .body(Some("missing target".to_string()))
                    .expect("builder"));
            }
            Ok(resp)
        },
    )
    .await?;

    let (host, port) = target.expect("validated during handshake");
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    proxy(ws_stream, tcp).await?;
    Ok(())
}

fn normalize_host(host: &str) -> String {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .to_string()
}

fn parse_target(target: &str) -> Option<(String, u16)> {
    let target = target.trim();
    if let Some(rest) = target.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = rest.strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host.to_string(), port));
    }

    let (host, port) = target.rsplit_once(':')?;
    if host.contains(':') {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    Some((host.to_string(), port))
}

async fn proxy(ws: WebSocketStream<TcpStream>, mut tcp: TcpStream) -> anyhow::Result<()> {
    let (ws_sink, mut ws_stream) = ws.split();
    let (mut tcp_reader, mut tcp_writer) = tcp.split();

    let (ws_out_tx, mut ws_out_rx) = mpsc::channel::<Message>(32);
    let ws_writer = tokio::spawn(async move {
        let mut ws_sink = ws_sink;
        while let Some(msg) = ws_out_rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        tcp_writer.write_all(&data).await?;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let _ = ws_out_tx.send(Message::Close(frame)).await;
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = ws_out_tx.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {
                        let _ = ws_out_tx.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            n = tcp_reader.read(&mut buf) => {
                match n {
                    Ok(0) => {
                        let _ = ws_out_tx.send(Message::Close(None)).await;
                        break;
                    }
                    Ok(n) => {
                        let _ = ws_out_tx.send(Message::Binary(buf[..n].to_vec().into())).await;
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
    }

    drop(ws_out_tx);
    let _ = ws_writer.await;

    Ok(())
}
