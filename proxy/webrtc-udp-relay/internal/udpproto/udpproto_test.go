package udpproto

import (
	"bytes"
	"errors"
	"net/netip"
	"testing"
)

func TestEncodeDecodeRoundTrip(t *testing.T) {
	c, err := NewCodec(64)
	if err != nil {
		t.Fatalf("NewCodec: %v", err)
	}

	in := datagram{
		GuestPort:  12345,
		RemoteIP:   [4]byte{1, 2, 3, 4},
		RemotePort: 443,
		Payload:    []byte("hello"),
	}

	buf := make([]byte, 0, 128)
	encoded, err := c.encodeDatagram(in, buf)
	if err != nil {
		t.Fatalf("EncodeDatagram: %v", err)
	}

	out, err := c.decodeDatagram(encoded)
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
	for n := 0; n < v1HeaderLen; n++ {
		_, err := DefaultCodec.decodeDatagram(make([]byte, n))
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
	_, err = c.encodeDatagram(datagram{
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
	_, err = c.decodeDatagram(frame)
	if !errors.Is(err, ErrPayloadTooLarge) {
		t.Fatalf("decode: got err=%v, want ErrPayloadTooLarge", err)
	}
}

func TestMaxFrameOverheadBytes_MatchesV2IPv6Encoding(t *testing.T) {
	// Ensure MaxFrameOverheadBytes stays in sync with the actual v2 header length
	// (IPv6 is the worst case).
	b, err := DefaultCodec.EncodeFrameV2(Frame{
		GuestPort:  1,
		RemoteIP:   netip.MustParseAddr("2001:db8::1"),
		RemotePort: 2,
		Payload:    nil,
	})
	if err != nil {
		t.Fatalf("EncodeFrameV2: %v", err)
	}
	if len(b) != MaxFrameOverheadBytes {
		t.Fatalf("len(encoded)=%d, want %d", len(b), MaxFrameOverheadBytes)
	}
}
