package l2tunnel

// Keep these constants in sync with:
// - docs/l2-tunnel-protocol.md
// - crates/aero-l2-protocol/src/lib.rs
// - web/src/shared/l2TunnelProtocol.ts

const (
	// Subprotocol is the WebSocket subprotocol name required for L2 tunnel framing.
	Subprotocol = "aero-l2-tunnel-v1"
	// DataChannelLabel is the WebRTC DataChannel label used for the L2 tunnel
	// transport.
	DataChannelLabel = "l2"
	// TokenSubprotocolPrefix is the optional additional offered subprotocol used for
	// upgrade-time authentication: `aero-l2-token.<token>`.
	//
	// The negotiated subprotocol must still be `aero-l2-tunnel-v1`.
	TokenSubprotocolPrefix = "aero-l2-token."

	// Header constants for the L2 tunnel framing protocol.
	Magic   byte = 0xA2
	Version byte = 0x03

	TypeFrame byte = 0x00
	TypePing  byte = 0x01
	TypePong  byte = 0x02
	TypeError byte = 0x7F

	HeaderLen = 4
)
