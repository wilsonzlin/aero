package udpproto

import (
	"encoding/binary"
	"errors"
	"fmt"
)

const (
	// HeaderLen is the number of bytes in a v1 datagram frame header.
	HeaderLen = 8

	// DefaultMaxPayload is a conservative default payload limit to reduce the chance
	// of fragmentation on the public Internet when running on top of WebRTC
	// (DTLS/SCTP/UDP).
	//
	// Deployments may choose to raise this (e.g. for LAN-only use) but MUST keep it
	// in sync on both sides of the relay.
	DefaultMaxPayload = 1200
)

var (
	ErrTooShort        = errors.New("udpproto: datagram frame too short")
	ErrPayloadTooLarge = errors.New("udpproto: payload too large")
)

// Datagram is the v1 binary frame payload carried in the WebRTC DataChannel.
//
// The GuestPort field always refers to the guest-side UDP port:
//   - outbound (guest -> remote): source port
//   - inbound (remote -> guest): destination port
//
// RemoteIP/RemotePort identify the remote endpoint (destination on outbound, source on inbound).
type Datagram struct {
	GuestPort  uint16
	RemoteIP   [4]byte
	RemotePort uint16
	Payload    []byte
}

// Codec validates and encodes/decodes frames.
type Codec struct {
	// MaxPayload is the maximum number of payload bytes allowed in a frame.
	MaxPayload int
}

// DefaultCodec is used by the top-level EncodeDatagram/DecodeDatagram helpers.
var DefaultCodec = Codec{MaxPayload: DefaultMaxPayload}

func NewCodec(maxPayload int) (Codec, error) {
	if maxPayload < 0 {
		return Codec{}, fmt.Errorf("udpproto: max payload must be >= 0")
	}
	return Codec{MaxPayload: maxPayload}, nil
}

func EncodeDatagram(d Datagram, dst []byte) ([]byte, error) {
	return DefaultCodec.EncodeDatagram(d, dst)
}

func DecodeDatagram(b []byte) (Datagram, error) {
	return DefaultCodec.DecodeDatagram(b)
}

func (c Codec) EncodeDatagram(d Datagram, dst []byte) ([]byte, error) {
	if c.MaxPayload < 0 {
		return nil, fmt.Errorf("udpproto: invalid codec max payload %d", c.MaxPayload)
	}
	if len(d.Payload) > c.MaxPayload {
		return nil, fmt.Errorf("%w: %d > %d", ErrPayloadTooLarge, len(d.Payload), c.MaxPayload)
	}

	n := HeaderLen + len(d.Payload)
	start := len(dst)

	if cap(dst) < start+n {
		grown := make([]byte, start, start+n)
		copy(grown, dst)
		dst = grown
	}
	dst = dst[:start+n]

	binary.BigEndian.PutUint16(dst[start:start+2], d.GuestPort)
	copy(dst[start+2:start+6], d.RemoteIP[:])
	binary.BigEndian.PutUint16(dst[start+6:start+8], d.RemotePort)
	copy(dst[start+8:start+n], d.Payload)

	return dst, nil
}

func (c Codec) DecodeDatagram(b []byte) (Datagram, error) {
	if c.MaxPayload < 0 {
		return Datagram{}, fmt.Errorf("udpproto: invalid codec max payload %d", c.MaxPayload)
	}
	if len(b) < HeaderLen {
		return Datagram{}, ErrTooShort
	}
	payload := b[HeaderLen:]
	if len(payload) > c.MaxPayload {
		return Datagram{}, fmt.Errorf("%w: %d > %d", ErrPayloadTooLarge, len(payload), c.MaxPayload)
	}

	return Datagram{
		GuestPort:  binary.BigEndian.Uint16(b[0:2]),
		RemoteIP:   [4]byte{b[2], b[3], b[4], b[5]},
		RemotePort: binary.BigEndian.Uint16(b[6:8]),
		Payload:    payload,
	}, nil
}
