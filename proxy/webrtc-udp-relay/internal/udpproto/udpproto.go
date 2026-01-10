package udpproto

import (
	"encoding/binary"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"strconv"
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

	// v2 frame prefix.
	V2Magic   = 0xA2
	V2Version = 0x02

	AFIPv4 = 0x04
	AFIPv6 = 0x06

	// v2 message types.
	V2TypeDatagram = 0x00

	// PREFER_V2 controls whether the server prefers encoding outbound (UDP→client)
	// packets as v2 when it is safe to do so.
	//
	// Note: v2 is required for IPv6; this flag only affects IPv4.
	PREFER_V2 = "PREFER_V2"
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

// Frame represents a decoded UDP relay frame (v1 or v2).
//
// Version is 1 or 2 depending on the framing used.
type Frame struct {
	Version    uint8
	GuestPort  uint16
	RemoteIP   netip.Addr
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

// PreferV2FromEnv returns true when the PREFER_V2 env var is set to a truthy
// value (as parsed by strconv.ParseBool).
func PreferV2FromEnv() bool {
	v, ok := os.LookupEnv(PREFER_V2)
	if !ok {
		return false
	}
	b, err := strconv.ParseBool(v)
	if err != nil {
		return false
	}
	return b
}

// Decode parses a datagram frame using the v2 prefix heuristic:
//
//   - if b starts with (0xA2, 0x02), decode as v2
//   - otherwise decode as v1
func Decode(b []byte) (Frame, error) {
	return DefaultCodec.DecodeFrame(b)
}

// EncodeV1 encodes an IPv4-only v1 frame.
func EncodeV1(f Frame) ([]byte, error) {
	return DefaultCodec.EncodeFrameV1(f)
}

// EncodeV2 encodes a v2 frame (IPv4 or IPv6).
func EncodeV2(f Frame) ([]byte, error) {
	return DefaultCodec.EncodeFrameV2(f)
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

func (c Codec) DecodeFrame(b []byte) (Frame, error) {
	if c.MaxPayload < 0 {
		return Frame{}, fmt.Errorf("udpproto: invalid codec max payload %d", c.MaxPayload)
	}
	if len(b) >= 2 && b[0] == V2Magic && b[1] == V2Version {
		return c.decodeV2(b)
	}

	d, err := c.DecodeDatagram(b)
	if err != nil {
		return Frame{}, err
	}
	return Frame{
		Version:    1,
		GuestPort:  d.GuestPort,
		RemoteIP:   netip.AddrFrom4(d.RemoteIP),
		RemotePort: d.RemotePort,
		Payload:    d.Payload,
	}, nil
}

func (c Codec) EncodeFrameV1(f Frame) ([]byte, error) {
	if c.MaxPayload < 0 {
		return nil, fmt.Errorf("udpproto: invalid codec max payload %d", c.MaxPayload)
	}
	if !f.RemoteIP.Is4() {
		return nil, fmt.Errorf("udpproto: v1 only supports IPv4")
	}
	d := Datagram{
		GuestPort:  f.GuestPort,
		RemoteIP:   f.RemoteIP.As4(),
		RemotePort: f.RemotePort,
		Payload:    f.Payload,
	}
	return c.EncodeDatagram(d, nil)
}

func (c Codec) EncodeFrameV2(f Frame) ([]byte, error) {
	if c.MaxPayload < 0 {
		return nil, fmt.Errorf("udpproto: invalid codec max payload %d", c.MaxPayload)
	}
	if len(f.Payload) > c.MaxPayload {
		return nil, fmt.Errorf("%w: %d > %d", ErrPayloadTooLarge, len(f.Payload), c.MaxPayload)
	}

	var (
		af    byte
		ipLen int
	)
	switch {
	case f.RemoteIP.Is4():
		af = AFIPv4
		ipLen = 4
	case f.RemoteIP.Is6():
		af = AFIPv6
		ipLen = 16
	default:
		return nil, fmt.Errorf("udpproto: invalid remote ip")
	}

	headerLen := 4 + 2 + ipLen + 2
	out := make([]byte, headerLen+len(f.Payload))
	out[0] = V2Magic
	out[1] = V2Version
	out[2] = af
	out[3] = V2TypeDatagram
	binary.BigEndian.PutUint16(out[4:6], f.GuestPort)

	ipOff := 6
	switch af {
	case AFIPv4:
		ip4 := f.RemoteIP.As4()
		copy(out[ipOff:ipOff+4], ip4[:])
	case AFIPv6:
		ip16 := f.RemoteIP.As16()
		copy(out[ipOff:ipOff+16], ip16[:])
	}

	portOff := ipOff + ipLen
	binary.BigEndian.PutUint16(out[portOff:portOff+2], f.RemotePort)
	copy(out[headerLen:], f.Payload)
	return out, nil
}

func (c Codec) decodeV2(b []byte) (Frame, error) {
	if len(b) < 12 {
		return Frame{}, ErrTooShort
	}
	if b[3] != V2TypeDatagram {
		return Frame{}, fmt.Errorf("udpproto: v2 unsupported message type: 0x%02x", b[3])
	}

	af := b[2]
	guestPort := binary.BigEndian.Uint16(b[4:6])

	offset := 6
	var (
		remoteIP netip.Addr
		ipLen    int
	)
	switch af {
	case AFIPv4:
		ipLen = 4
		if len(b) < offset+ipLen+2 {
			return Frame{}, ErrTooShort
		}
		var ip4 [4]byte
		copy(ip4[:], b[offset:offset+4])
		remoteIP = netip.AddrFrom4(ip4)
	case AFIPv6:
		ipLen = 16
		if len(b) < offset+ipLen+2 {
			return Frame{}, ErrTooShort
		}
		var ip16 [16]byte
		copy(ip16[:], b[offset:offset+16])
		remoteIP = netip.AddrFrom16(ip16)
	default:
		return Frame{}, fmt.Errorf("udpproto: v2 unknown address family: 0x%02x", af)
	}
	offset += ipLen

	remotePort := binary.BigEndian.Uint16(b[offset : offset+2])
	offset += 2

	payload := b[offset:]
	if len(payload) > c.MaxPayload {
		return Frame{}, fmt.Errorf("%w: %d > %d", ErrPayloadTooLarge, len(payload), c.MaxPayload)
	}

	return Frame{
		Version:    2,
		GuestPort:  guestPort,
		RemoteIP:   remoteIP,
		RemotePort: remotePort,
		Payload:    payload,
	}, nil
}
