package udpproto

import (
	"bytes"
	"errors"
	"net/netip"
	"strings"
	"testing"
)

func TestEncodeDecodeRoundTrip(t *testing.T) {
	c, err := NewCodec(64)
	if err != nil {
		t.Fatalf("NewCodec: %v", err)
	}

	in := Datagram{
		GuestPort:  12345,
		RemoteIP:   [4]byte{1, 2, 3, 4},
		RemotePort: 443,
		Payload:    []byte("hello"),
	}

	buf := make([]byte, 0, 128)
	encoded, err := c.EncodeDatagram(in, buf)
	if err != nil {
		t.Fatalf("EncodeDatagram: %v", err)
	}

	out, err := c.DecodeDatagram(encoded)
	if err != nil {
		t.Fatalf("DecodeDatagram: %v", err)
	}

	if out.GuestPort != in.GuestPort {
		t.Fatalf("GuestPort: got %d want %d", out.GuestPort, in.GuestPort)
	}
	if out.RemoteIP != in.RemoteIP {
		t.Fatalf("RemoteIP: got %v want %v", out.RemoteIP, in.RemoteIP)
	}
	if out.RemotePort != in.RemotePort {
		t.Fatalf("RemotePort: got %d want %d", out.RemotePort, in.RemotePort)
	}
	if !bytes.Equal(out.Payload, in.Payload) {
		t.Fatalf("Payload: got %x want %x", out.Payload, in.Payload)
	}
}

func TestDecodeTooShort(t *testing.T) {
	for n := 0; n < HeaderLen; n++ {
		_, err := DecodeDatagram(make([]byte, n))
		if !errors.Is(err, ErrTooShort) {
			t.Fatalf("len=%d: got err=%v, want ErrTooShort", n, err)
		}
	}
}

func TestMaxPayloadEnforced(t *testing.T) {
	c, err := NewCodec(3)
	if err != nil {
		t.Fatalf("NewCodec: %v", err)
	}

	// Encode should reject payloads over max.
	_, err = c.EncodeDatagram(Datagram{
		GuestPort:  1,
		RemoteIP:   [4]byte{127, 0, 0, 1},
		RemotePort: 2,
		Payload:    []byte{0, 1, 2, 3},
	}, nil)
	if !errors.Is(err, ErrPayloadTooLarge) {
		t.Fatalf("encode: got err=%v, want ErrPayloadTooLarge", err)
	}

	// Decode should reject payloads over max.
	frame := []byte{
		0, 1, // guest_port
		127, 0, 0, 1, // remote_ipv4
		0, 2, // remote_port
		0, 1, 2, 3, // payload (4 bytes; max is 3)
	}
	_, err = c.DecodeDatagram(frame)
	if !errors.Is(err, ErrPayloadTooLarge) {
		t.Fatalf("decode: got err=%v, want ErrPayloadTooLarge", err)
	}
}

func TestGoldenVector(t *testing.T) {
	d := Datagram{
		GuestPort:  10000,
		RemoteIP:   [4]byte{192, 0, 2, 1},
		RemotePort: 53,
		Payload:    []byte("abc"),
	}

	got, err := EncodeDatagram(d, nil)
	if err != nil {
		t.Fatalf("EncodeDatagram: %v", err)
	}

	want := []byte{0x27, 0x10, 0xc0, 0x00, 0x02, 0x01, 0x00, 0x35, 0x61, 0x62, 0x63}
	if !bytes.Equal(got, want) {
		t.Fatalf("encoded bytes: got %x want %x", got, want)
	}

	decoded, err := DecodeDatagram(want)
	if err != nil {
		t.Fatalf("DecodeDatagram: %v", err)
	}
	if decoded.GuestPort != d.GuestPort || decoded.RemoteIP != d.RemoteIP || decoded.RemotePort != d.RemotePort || !bytes.Equal(decoded.Payload, d.Payload) {
		t.Fatalf("decoded datagram mismatch: got %#v, want %#v", decoded, d)
	}
}

func TestEncodeDecodeV2IPv6Vector(t *testing.T) {
	ip := netip.MustParseAddr("2001:db8::1")
	f := Frame{
		GuestPort:  0xBEEF,
		RemoteIP:   ip,
		RemotePort: 0xCAFE,
		Payload:    []byte{0x01, 0x02, 0x03},
	}

	got, err := EncodeV2(f)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	want := []byte{
		0xA2, 0x02, 0x06, 0x00,
		0xBE, 0xEF,
		0x20, 0x01, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
		0xCA, 0xFE,
		0x01, 0x02, 0x03,
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("EncodeV2 mismatch:\n got: %x\nwant: %x", got, want)
	}

	decoded, err := Decode(got)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if decoded.Version != 2 {
		t.Fatalf("decoded.Version = %d, want 2", decoded.Version)
	}
	if decoded.GuestPort != f.GuestPort || decoded.RemotePort != f.RemotePort || decoded.RemoteIP != f.RemoteIP {
		t.Fatalf("decoded header mismatch: %+v vs %+v", decoded, f)
	}
	if !bytes.Equal(decoded.Payload, f.Payload) {
		t.Fatalf("decoded payload = %x, want %x", decoded.Payload, f.Payload)
	}
}

func TestDecodeV2RejectsUnsupportedMessageType(t *testing.T) {
	pkt := []byte{
		0xA2, 0x02, 0x04, 0x01, // type must be 0x00
		0x00, 0x01,
		127, 0, 0, 1,
		0x00, 0x35,
	}
	_, err := Decode(pkt)
	if err == nil || !strings.Contains(err.Error(), "unsupported message type") {
		t.Fatalf("expected unsupported message type error, got %v", err)
	}
}

func TestDecodeV2RejectsUnknownAddressFamily(t *testing.T) {
	pkt := []byte{
		0xA2, 0x02, 0xFF, 0x00, // unknown AF
		0x00, 0x01,
		127, 0, 0, 1,
		0x00, 0x35,
	}
	_, err := Decode(pkt)
	if err == nil || !strings.Contains(err.Error(), "unknown address family") {
		t.Fatalf("expected unknown address family error, got %v", err)
	}
}
