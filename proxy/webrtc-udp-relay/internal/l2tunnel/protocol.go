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
	Magic   byte = 0xA2
	Version byte = 0x03

	TypeFrame byte = 0x00
	TypePing  byte = 0x01
	TypePong  byte = 0x02
	TypeError byte = 0x7F

	HeaderLen = 4

	// Recommended default payload limits (see docs/l2-tunnel-protocol.md).
	DefaultMaxFramePayload   = 2048
	DefaultMaxControlPayload = 256

	// ErrorStructuredHeaderLen is the structured ERROR payload header length:
	//   code(u16 BE) | msg_len(u16 BE)
	ErrorStructuredHeaderLen = 4

	// Structured ERROR payload codes (see docs/l2-tunnel-protocol.md).
	ErrorCodeProtocolError    = 1
	ErrorCodeAuthRequired     = 2
	ErrorCodeAuthInvalid      = 3
	ErrorCodeOriginMissing    = 4
	ErrorCodeOriginDenied     = 5
	ErrorCodeQuotaBytes       = 6
	ErrorCodeQuotaFPS         = 7
	ErrorCodeQuotaConnections = 8
	ErrorCodeBackpressure     = 9
)

// Limits control the maximum payload sizes accepted/encoded for L2 tunnel
// messages.
type Limits struct {
	MaxFramePayload   int
	MaxControlPayload int
}

func (l Limits) maxPayloadForType(msgType byte) int {
	if msgType == TypeFrame {
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
var DefaultLimits = Limits{
	MaxFramePayload:   DefaultMaxFramePayload,
	MaxControlPayload: DefaultMaxControlPayload,
}

// Message is a decoded L2 tunnel message.
type Message struct {
	Version byte
	Type    byte
	Flags   byte
	Payload []byte
}

type DecodeErrorCode string

const (
	DecodeErrorTooShort           DecodeErrorCode = "too_short"
	DecodeErrorInvalidMagic       DecodeErrorCode = "invalid_magic"
	DecodeErrorUnsupportedVersion DecodeErrorCode = "unsupported_version"
	DecodeErrorPayloadTooLarge    DecodeErrorCode = "payload_too_large"
)

// DecodeError reports why an L2 tunnel message failed to decode.
type DecodeError struct {
	Code    DecodeErrorCode
	Message string
}

func (e *DecodeError) Error() string {
	return e.Message
}

// EncodeWithLimits encodes an L2 tunnel message (header + payload) while
// enforcing the provided payload limits.
func EncodeWithLimits(msgType byte, flags byte, payload []byte, limits Limits) ([]byte, error) {
	max := limits.maxPayloadForType(msgType)
	if len(payload) > max {
		return nil, fmt.Errorf("payload too large: %d > %d", len(payload), max)
	}

	out := make([]byte, HeaderLen+len(payload))
	out[0] = Magic
	out[1] = Version
	out[2] = msgType
	out[3] = flags
	copy(out[HeaderLen:], payload)
	return out, nil
}

// DecodeWithLimits decodes an L2 tunnel message while enforcing the provided
// payload limits.
func DecodeWithLimits(buf []byte, limits Limits) (Message, error) {
	if len(buf) < HeaderLen {
		return Message{}, &DecodeError{
			Code:    DecodeErrorTooShort,
			Message: fmt.Sprintf("message too short: %d < %d", len(buf), HeaderLen),
		}
	}
	if buf[0] != Magic {
		return Message{}, &DecodeError{
			Code:    DecodeErrorInvalidMagic,
			Message: fmt.Sprintf("invalid magic: 0x%02x", buf[0]),
		}
	}
	if buf[1] != Version {
		return Message{}, &DecodeError{
			Code:    DecodeErrorUnsupportedVersion,
			Message: fmt.Sprintf("unsupported version: 0x%02x", buf[1]),
		}
	}

	msgType := buf[2]
	flags := buf[3]
	payload := buf[HeaderLen:]
	max := limits.maxPayloadForType(msgType)
	if len(payload) > max {
		return Message{}, &DecodeError{
			Code:    DecodeErrorPayloadTooLarge,
			Message: fmt.Sprintf("payload too large: %d > %d", len(payload), max),
		}
	}

	return Message{
		Version: buf[1],
		Type:    msgType,
		Flags:   flags,
		Payload: payload,
	}, nil
}

// EncodeFrame encodes a FRAME message using DefaultLimits.
func EncodeFrame(payload []byte) ([]byte, error) {
	return EncodeWithLimits(TypeFrame, 0, payload, DefaultLimits)
}

// EncodePing encodes a PING message using DefaultLimits.
func EncodePing(payload []byte) ([]byte, error) {
	return EncodeWithLimits(TypePing, 0, payload, DefaultLimits)
}

// EncodePong encodes a PONG message using DefaultLimits.
func EncodePong(payload []byte) ([]byte, error) {
	return EncodeWithLimits(TypePong, 0, payload, DefaultLimits)
}

// EncodeErrorMessage encodes an ERROR message using DefaultLimits.
func EncodeErrorMessage(payload []byte) ([]byte, error) {
	return EncodeWithLimits(TypeError, 0, payload, DefaultLimits)
}

// DecodeMessage decodes an L2 tunnel message using DefaultLimits.
func DecodeMessage(buf []byte) (Message, error) {
	return DecodeWithLimits(buf, DefaultLimits)
}

// EncodeStructuredErrorPayload encodes a structured ERROR payload:
//
//	code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
//
// The returned payload is truncated as needed to fit within maxPayloadBytes.
func EncodeStructuredErrorPayload(code uint16, message string, maxPayloadBytes int) []byte {
	if maxPayloadBytes < 0 {
		maxPayloadBytes = 0
	}
	if maxPayloadBytes < ErrorStructuredHeaderLen {
		return []byte{}
	}

	maxMsgLen := maxPayloadBytes - ErrorStructuredHeaderLen
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

	out := make([]byte, ErrorStructuredHeaderLen+len(msgBytes))
	binary.BigEndian.PutUint16(out[0:2], code)
	binary.BigEndian.PutUint16(out[2:4], uint16(len(msgBytes)))
	copy(out[ErrorStructuredHeaderLen:], msgBytes)
	return out
}

// DecodeStructuredErrorPayload attempts to decode a structured ERROR payload.
//
// It returns (code, message, true) only if the payload matches the exact
// structured encoding and the message bytes are valid UTF-8.
func DecodeStructuredErrorPayload(payload []byte) (uint16, string, bool) {
	if len(payload) < ErrorStructuredHeaderLen {
		return 0, "", false
	}
	code := binary.BigEndian.Uint16(payload[0:2])
	msgLen := int(binary.BigEndian.Uint16(payload[2:4]))
	if len(payload) != ErrorStructuredHeaderLen+msgLen {
		return 0, "", false
	}
	msgBytes := payload[ErrorStructuredHeaderLen:]
	if !utf8.Valid(msgBytes) {
		return 0, "", false
	}
	return code, string(msgBytes), true
}
