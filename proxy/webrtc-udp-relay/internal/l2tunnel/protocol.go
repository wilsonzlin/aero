package l2tunnel

import (
	"encoding/binary"
	"fmt"
	"unicode/utf8"
)

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
	magic   byte = 0xA2
	version byte = 0x03

	typeFrame byte = 0x00
	typePing  byte = 0x01
	typePong  byte = 0x02
	typeError byte = 0x7F

	headerLen = 4

	// Recommended default payload limits (see docs/l2-tunnel-protocol.md).
	defaultMaxFramePayload   = 2048
	defaultMaxControlPayload = 256

	// ErrorStructuredHeaderLen is the structured ERROR payload header length:
	//   code(u16 BE) | msg_len(u16 BE)
	errorStructuredHeaderLen = 4

	// Structured ERROR payload codes (see docs/l2-tunnel-protocol.md).
	errorCodeProtocolError    = 1
	errorCodeAuthRequired     = 2
	errorCodeAuthInvalid      = 3
	errorCodeOriginMissing    = 4
	errorCodeOriginDenied     = 5
	errorCodeQuotaBytes       = 6
	errorCodeQuotaFPS         = 7
	errorCodeQuotaConnections = 8
	errorCodeBackpressure     = 9
)

// limits control the maximum payload sizes accepted/encoded for L2 tunnel
// messages.
type limits struct {
	MaxFramePayload   int
	MaxControlPayload int
}

func (l limits) maxPayloadForType(msgType byte) int {
	if msgType == typeFrame {
		if l.MaxFramePayload < 0 {
			return 0
		}
		return l.MaxFramePayload
	}
	if l.MaxControlPayload < 0 {
		return 0
	}
	return l.MaxControlPayload
}

// DefaultLimits matches the recommended default limits in the spec.
var DefaultLimits = limits{
	MaxFramePayload:   defaultMaxFramePayload,
	MaxControlPayload: defaultMaxControlPayload,
}

// message is a decoded L2 tunnel message.
type message struct {
	Version byte
	Type    byte
	Flags   byte
	Payload []byte
}

type decodeErrorCode string

const (
	decodeErrorTooShort           decodeErrorCode = "too_short"
	decodeErrorInvalidMagic       decodeErrorCode = "invalid_magic"
	decodeErrorUnsupportedVersion decodeErrorCode = "unsupported_version"
	decodeErrorPayloadTooLarge    decodeErrorCode = "payload_too_large"
)

// decodeError reports why an L2 tunnel message failed to decode.
type decodeError struct {
	Code    decodeErrorCode
	Message string
}

func (e *decodeError) Error() string {
	return e.Message
}

// EncodeWithLimits encodes an L2 tunnel message (header + payload) while
// enforcing the provided payload limits.
func EncodeWithLimits(msgType byte, flags byte, payload []byte, limits limits) ([]byte, error) {
	max := limits.maxPayloadForType(msgType)
	if len(payload) > max {
		return nil, fmt.Errorf("payload too large: %d > %d", len(payload), max)
	}

	out := make([]byte, headerLen+len(payload))
	out[0] = magic
	out[1] = version
	out[2] = msgType
	out[3] = flags
	copy(out[headerLen:], payload)
	return out, nil
}

// decodeWithLimits decodes an L2 tunnel message while enforcing the provided
// payload limits.
func decodeWithLimits(buf []byte, limits limits) (message, error) {
	if len(buf) < headerLen {
		return message{}, &decodeError{
			Code:    decodeErrorTooShort,
			Message: fmt.Sprintf("message too short: %d < %d", len(buf), headerLen),
		}
	}
	if buf[0] != magic {
		return message{}, &decodeError{
			Code:    decodeErrorInvalidMagic,
			Message: fmt.Sprintf("invalid magic: 0x%02x", buf[0]),
		}
	}
	if buf[1] != version {
		return message{}, &decodeError{
			Code:    decodeErrorUnsupportedVersion,
			Message: fmt.Sprintf("unsupported version: 0x%02x", buf[1]),
		}
	}

	msgType := buf[2]
	flags := buf[3]
	payload := buf[headerLen:]
	max := limits.maxPayloadForType(msgType)
	if len(payload) > max {
		return message{}, &decodeError{
			Code:    decodeErrorPayloadTooLarge,
			Message: fmt.Sprintf("payload too large: %d > %d", len(payload), max),
		}
	}

	return message{
		Version: buf[1],
		Type:    msgType,
		Flags:   flags,
		Payload: payload,
	}, nil
}

// DecodeMessage decodes an L2 tunnel message using DefaultLimits.
func DecodeMessage(buf []byte) (message, error) {
	return decodeWithLimits(buf, DefaultLimits)
}

// encodeStructuredErrorPayload encodes a structured ERROR payload:
//
//	code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
//
// The returned payload is truncated as needed to fit within maxPayloadBytes.
func encodeStructuredErrorPayload(code uint16, message string, maxPayloadBytes int) []byte {
	if maxPayloadBytes < 0 {
		maxPayloadBytes = 0
	}
	if maxPayloadBytes < errorStructuredHeaderLen {
		return []byte{}
	}

	maxMsgLen := maxPayloadBytes - errorStructuredHeaderLen
	if maxMsgLen > 0xffff {
		maxMsgLen = 0xffff
	}

	msgBytes := []byte(message)
	if len(msgBytes) > maxMsgLen {
		msgBytes = msgBytes[:maxMsgLen]
		// Ensure we do not return invalid UTF-8 (best-effort); this should only
		// need to trim up to 3 bytes for a multi-byte rune boundary.
		for len(msgBytes) > 0 && !utf8.Valid(msgBytes) {
			msgBytes = msgBytes[:len(msgBytes)-1]
		}
	}

	out := make([]byte, errorStructuredHeaderLen+len(msgBytes))
	binary.BigEndian.PutUint16(out[0:2], code)
	binary.BigEndian.PutUint16(out[2:4], uint16(len(msgBytes)))
	copy(out[errorStructuredHeaderLen:], msgBytes)
	return out
}

// decodeStructuredErrorPayload attempts to decode a structured ERROR payload.
//
// It returns (code, message, true) only if the payload matches the exact
// structured encoding and the message bytes are valid UTF-8.
func decodeStructuredErrorPayload(payload []byte) (uint16, string, bool) {
	if len(payload) < errorStructuredHeaderLen {
		return 0, "", false
	}
	code := binary.BigEndian.Uint16(payload[0:2])
	msgLen := int(binary.BigEndian.Uint16(payload[2:4]))
	if len(payload) != errorStructuredHeaderLen+msgLen {
		return 0, "", false
	}
	msgBytes := payload[errorStructuredHeaderLen:]
	if !utf8.Valid(msgBytes) {
		return 0, "", false
	}
	return code, string(msgBytes), true
}
