package udpproto

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/netip"
	"os"
	"path/filepath"
	"runtime"
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
	// Use the canonical v1 golden vector and extend the payload by one byte.
	vectors := loadNetworkingVectors(t)
	frame := append(mustDecodeHex(t, vectors.UDPRelay.V1.FrameHex), 0x00)
	_, err = c.DecodeDatagram(frame)
	if !errors.Is(err, ErrPayloadTooLarge) {
		t.Fatalf("decode: got err=%v, want ErrPayloadTooLarge", err)
	}
}

func TestGoldenVector(t *testing.T) {
	vectors := loadNetworkingVectors(t)
	v := vectors.UDPRelay.V1
	if len(v.RemoteIpv4) != 4 {
		t.Fatalf("v1 remoteIpv4 length=%d, want 4", len(v.RemoteIpv4))
	}

	d := Datagram{
		GuestPort: uint16(v.GuestPort),
		RemoteIP: [4]byte{
			byte(v.RemoteIpv4[0]),
			byte(v.RemoteIpv4[1]),
			byte(v.RemoteIpv4[2]),
			byte(v.RemoteIpv4[3]),
		},
		RemotePort: uint16(v.RemotePort),
		Payload:    []byte(v.PayloadUtf8),
	}

	got, err := EncodeDatagram(d, nil)
	if err != nil {
		t.Fatalf("EncodeDatagram: %v", err)
	}

	want := mustDecodeHex(t, v.FrameHex)
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
	vectors := loadNetworkingVectors(t)
	v := vectors.UDPRelay.V2IPv6

	remoteIPBytes := mustDecodeHex(t, v.RemoteIPHex)
	if len(remoteIPBytes) != 16 {
		t.Fatalf("v2 remoteIpHex length=%d bytes, want 16", len(remoteIPBytes))
	}
	var ip16 [16]byte
	copy(ip16[:], remoteIPBytes)
	ip := netip.AddrFrom16(ip16)

	f := Frame{
		GuestPort:  uint16(v.GuestPort),
		RemoteIP:   ip,
		RemotePort: uint16(v.RemotePort),
		Payload:    mustDecodeHex(t, v.PayloadHex),
	}

	got, err := EncodeV2(f)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	want := mustDecodeHex(t, v.FrameHex)
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
	vectors := loadNetworkingVectors(t)
	pkt := append([]byte(nil), mustDecodeHex(t, vectors.UDPRelay.V2IPv6.FrameHex)...)
	pkt[3] = 0x01 // type must be 0x00
	_, err := Decode(pkt)
	if err == nil || !strings.Contains(err.Error(), "unsupported message type") {
		t.Fatalf("expected unsupported message type error, got %v", err)
	}
}

func TestDecodeV2RejectsUnknownAddressFamily(t *testing.T) {
	vectors := loadNetworkingVectors(t)
	pkt := append([]byte(nil), mustDecodeHex(t, vectors.UDPRelay.V2IPv6.FrameHex)...)
	pkt[2] = 0xFF // unknown AF
	_, err := Decode(pkt)
	if err == nil || !strings.Contains(err.Error(), "unknown address family") {
		t.Fatalf("expected unknown address family error, got %v", err)
	}
}

type networkingVectors struct {
	SchemaVersion int `json:"schemaVersion"`
	UDPRelay      struct {
		V1 struct {
			GuestPort   int    `json:"guestPort"`
			RemoteIpv4  []int  `json:"remoteIpv4"`
			RemotePort  int    `json:"remotePort"`
			PayloadUtf8 string `json:"payloadUtf8"`
			FrameHex    string `json:"frameHex"`
		} `json:"v1"`
		V2IPv6 struct {
			GuestPort     int    `json:"guestPort"`
			AddressFamily int    `json:"addressFamily"`
			RemoteIPHex   string `json:"remoteIpHex"`
			RemotePort    int    `json:"remotePort"`
			PayloadHex    string `json:"payloadHex"`
			FrameHex      string `json:"frameHex"`
		} `json:"v2_ipv6"`
	} `json:"udpRelay"`
}

func loadNetworkingVectors(t *testing.T) networkingVectors {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}

	// This test lives in:
	//   proxy/webrtc-udp-relay/internal/udpproto/
	// The shared vectors live at:
	//   tests/protocol-vectors/networking.json
	vectorPath := filepath.Join(filepath.Dir(thisFile), "../../../..", "tests", "protocol-vectors", "networking.json")
	raw, err := os.ReadFile(vectorPath)
	if err != nil {
		t.Fatalf("read vectors %q: %v", vectorPath, err)
	}

	var v networkingVectors
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatalf("parse vectors %q: %v", vectorPath, err)
	}
	if v.SchemaVersion != 1 {
		t.Fatalf("unexpected vectors schemaVersion=%d (want 1)", v.SchemaVersion)
	}
	return v
}

func mustDecodeHex(t *testing.T, s string) []byte {
	t.Helper()

	b, err := hex.DecodeString(s)
	if err != nil {
		t.Fatalf("decode hex %q: %v", s, err)
	}
	return b
}
