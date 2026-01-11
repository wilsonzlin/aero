use std::net::SocketAddr;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};

const WS_SUBPROTOCOL: &str = "aero-l2-tunnel-v1";
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8090";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let addr = std::env::var("AERO_L2_PROXY_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string());
    let addr: SocketAddr = addr
        .parse()
        .unwrap_or_else(|_| panic!("invalid AERO_L2_PROXY_LISTEN_ADDR: {addr}"));

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/l2", get(l2_ws));

    tracing::info!("aero-l2-proxy listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listen addr");
    axum::serve(listener, app).await.expect("serve");
}

async fn l2_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
    // Enforce subprotocol negotiation per docs/l2-tunnel-protocol.md.
    ws.protocols([WS_SUBPROTOCOL]).on_upgrade(handle_l2_socket)
}

async fn handle_l2_socket(mut socket: WebSocket) {
    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(err) => {
                tracing::debug!("websocket recv error: {err}");
                break;
            }
        };

        match msg {
            Message::Binary(buf) => {
                // For now, we only implement the keepalive control plane:
                // respond to protocol-level PING with a PONG echo.
                match aero_l2_protocol::decode_message(&buf) {
                    Ok(decoded) => {
                        if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_PING {
                            match aero_l2_protocol::encode_pong(Some(decoded.payload)) {
                                Ok(pong) => {
                                    if socket.send(Message::Binary(pong)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    tracing::debug!("failed to encode pong: {err}");
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::debug!("dropping invalid l2 message: {err}");
                        // Send a best-effort ERROR control message (bounded by control payload limit).
                        let msg = err.to_string();
                        let payload = msg.as_bytes();
                        let payload = if payload.len()
                            > aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD
                        {
                            &payload[..aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD]
                        } else {
                            payload
                        };

                        if let Ok(wire) = aero_l2_protocol::encode_with_limits(
                            aero_l2_protocol::L2_TUNNEL_TYPE_ERROR,
                            0,
                            payload,
                            &Default::default(),
                        ) {
                            let _ = socket.send(Message::Binary(wire)).await;
                        }
                        break;
                    }
                }
            }
            Message::Close(_) => break,
            // Ignore text frames (wscat often sends these interactively).
            _ => {}
        }
    }
}
