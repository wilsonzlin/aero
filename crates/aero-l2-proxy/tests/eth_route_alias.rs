#![cfg(not(target_arch = "wasm32"))]

use std::net::Ipv4Addr;

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
use futures_util::{SinkExt, StreamExt};
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eth_route_is_alias_for_l2() {
    // Ensure local developer env vars don't accidentally harden the proxy in tests.
    std::env::remove_var("AERO_L2_ALLOWED_ORIGINS");
    std::env::remove_var("ALLOWED_ORIGINS");
    std::env::remove_var("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    std::env::remove_var("AERO_L2_ALLOWED_HOSTS");
    std::env::remove_var("AERO_L2_TRUST_PROXY_HOST");
    std::env::remove_var("AERO_L2_API_KEY");
    std::env::remove_var("AERO_L2_TOKEN");
    std::env::remove_var("AERO_L2_SESSION_SECRET");
    std::env::remove_var("SESSION_SECRET");
    std::env::remove_var("AERO_GATEWAY_SESSION_SECRET");
    std::env::remove_var("AERO_L2_JWT_SECRET");
    std::env::remove_var("AERO_L2_JWT_AUDIENCE");
    std::env::remove_var("AERO_L2_JWT_ISSUER");
    std::env::remove_var("AERO_L2_INSECURE_ALLOW_NO_AUTH");

    // Start the proxy in open/unauthenticated mode so the test can focus on the /eth route
    // compatibility layer.
    std::env::set_var("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    std::env::set_var("AERO_L2_OPEN", "1");
    std::env::set_var("AERO_L2_AUTH_MODE", "none");
    std::env::set_var("AERO_L2_MAX_CONNECTIONS", "0");
    std::env::set_var("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "0");
    std::env::set_var("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    std::env::set_var("AERO_L2_MAX_FRAMES_PER_SECOND", "0");
    std::env::set_var("AERO_L2_PING_INTERVAL_MS", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let proxy_addr = proxy.local_addr();

    let ws_url = format!("ws://{proxy_addr}/eth");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);

    // Send an ARP request for the gateway; the proxy should respond with an ARP reply if the tunnel
    // is functioning correctly.
    let arp_req = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: guest_mac,
        sender_ip: guest_ip,
        target_mac: MacAddr([0, 0, 0, 0, 0, 0]),
        target_ip: gateway_ip,
    }
    .build_vec()
    .expect("build ARP request");
    let arp_frame = EthernetFrameBuilder {
        dest_mac: MacAddr::BROADCAST,
        src_mac: guest_mac,
        ethertype: EtherType::ARP,
        payload: &arp_req,
    }
    .build_vec()
    .expect("build ARP Ethernet frame");
    ws_tx
        .send(Message::Binary(
            aero_l2_protocol::encode_frame(&arp_frame).unwrap().into(),
        ))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = ws_rx.next().await.expect("ws closed").unwrap();
            let Message::Binary(data) = msg else {
                continue;
            };
            let Ok(decoded) = aero_l2_protocol::decode_message(&data) else {
                continue;
            };
            if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_FRAME {
                continue;
            }

            let Ok(eth) = EthernetFrame::parse(decoded.payload) else {
                continue;
            };
            if eth.ethertype() != EtherType::ARP {
                continue;
            }
            let Ok(arp) = ArpPacket::parse(eth.payload()) else {
                continue;
            };
            if arp.opcode() == ARP_OP_REPLY && arp.sender_ip() == Some(gateway_ip) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for ARP reply");

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}
