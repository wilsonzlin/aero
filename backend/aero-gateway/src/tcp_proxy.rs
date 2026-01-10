use std::{
    net::{IpAddr, SocketAddr},
    time::SystemTime,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, State,
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    capture::{ConnectionMeta, Direction},
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct TcpProxyQuery {
    pub target: String,
    pub session: Option<String>,
}

pub async fn tcp_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<TcpProxyQuery>,
    headers: HeaderMap,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    let client_ip = connect_info
        .map(|ConnectInfo(addr)| addr.ip())
        .or_else(|| x_forwarded_for(&headers));

    ws.on_upgrade(move |socket| async move {
        handle_tcp_ws(socket, state, query, client_ip).await;
    })
}

fn x_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    let value = headers.get("x-forwarded-for")?.to_str().ok()?;
    let first = value.split(',').next()?.trim();
    first.parse().ok()
}

async fn handle_tcp_ws(
    socket: WebSocket,
    state: AppState,
    query: TcpProxyQuery,
    client_ip: Option<IpAddr>,
) {
    let (host, port) = match parse_target(&query.target) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("invalid tcp target {:?}: {err}", query.target);
            return;
        }
    };

    let conn_id = state.stats.next_connection_id();
    state.stats.tcp_connection_opened();

    let capture = match state
        .capture
        .open_connection_capture(ConnectionMeta {
            connection_id: conn_id,
            started_at: SystemTime::now(),
            client_ip,
            session_secret: query.session.as_deref(),
            target: &query.target,
        })
        .await
    {
        Ok(capture) => capture,
        Err(err) => {
            tracing::warn!("failed to initialise capture for conn {conn_id}: {err}");
            None
        }
    };

    let tcp = match state.dns_cache.connect(&host, port).await {
        Ok(tcp) => tcp,
        Err(err) => {
            tracing::warn!(
                "failed to connect tcp target {}:{} for conn {conn_id}: {err}",
                host,
                port
            );
            state.stats.tcp_connection_closed();
            if let Some(capture) = capture.as_ref() {
                let _ = capture.close().await;
            }
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (mut tcp_reader, mut tcp_writer) = tcp.into_split();

    let stats_c2t = state.stats.clone();
    let capture_c2t = capture.clone();
    let c2t = async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(data) => {
                    if tcp_writer.write_all(&data).await.is_err() {
                        break;
                    }
                    stats_c2t.add_bytes_client_to_target(data.len() as u64);
                    if let Some(capture) = capture_c2t.as_ref() {
                        let _ = capture.record(Direction::ClientToTarget, &data).await;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        let _ = tcp_writer.shutdown().await;
    };

    let stats_t2c = state.stats.clone();
    let capture_t2c = capture.clone();
    let t2c = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match tcp_reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };

            if ws_sender
                .send(Message::Binary(buf[..n].to_vec()))
                .await
                .is_err()
            {
                break;
            }

            stats_t2c.add_bytes_target_to_client(n as u64);
            if let Some(capture) = capture_t2c.as_ref() {
                let _ = capture.record(Direction::TargetToClient, &buf[..n]).await;
            }
        }
    };

    tokio::join!(c2t, t2c);

    state.stats.tcp_connection_closed();
    if let Some(capture) = capture.as_ref() {
        let _ = capture.close().await;
    }
}

fn parse_target(target: &str) -> Result<(String, u16), &'static str> {
    if let Some(rest) = target.strip_prefix('[') {
        let Some((host, rest)) = rest.split_once(']') else {
            return Err("missing closing bracket in IPv6 address");
        };
        let Some(port) = rest.strip_prefix(':') else {
            return Err("missing :port suffix");
        };
        let port: u16 = port.parse().map_err(|_| "invalid port")?;
        return Ok((host.to_string(), port));
    }

    let Some((host, port)) = target.rsplit_once(':') else {
        return Err("missing :port suffix");
    };
    let port: u16 = port.parse().map_err(|_| "invalid port")?;
    Ok((host.to_string(), port))
}
