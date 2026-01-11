package udpproto

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

type udpRelayVectorFile struct {
	Schema  int              `json:"schema"`
	Vectors []udpRelayVector `json:"vectors"`
}

type udpRelayVector struct {
	Name string `json:"name"`

	Version   uint8  `json:"version"`
	FrameB64  string `json:"frame_b64"`
	GuestPort uint16 `json:"guestPort"`
	RemoteIP  string `json:"remoteIp"`
	RemotePort uint16 `json:"remotePort"`
	PayloadB64 string `json:"payload_b64"`

	ExpectError   bool   `json:"expectError"`
	ErrorContains string `json:"errorContains"`
}

func findRepoRoot(startDir string) (string, error) {
	dir := startDir
	for {
		// Heuristics: repo root always has AGENTS.md, and also currently has a Cargo.toml.
		for _, marker := range []string{"AGENTS.md", "Cargo.toml"} {
			if _, err := os.Stat(filepath.Join(dir, marker)); err == nil {
				return dir, nil
			}
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("repo root not found from %s", startDir)
		}
		dir = parent
	}
}

func loadUdpRelayVectors(t *testing.T) udpRelayVectorFile {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}

	root, err := findRepoRoot(filepath.Dir(thisFile))
	if err != nil {
		t.Fatalf("find repo root: %v", err)
	}

	b, err := os.ReadFile(filepath.Join(root, "protocol-vectors", "udp-relay.json"))
	if err != nil {
		t.Fatalf("read udp-relay.json: %v", err)
	}

	var vf udpRelayVectorFile
	if err := json.Unmarshal(b, &vf); err != nil {
		t.Fatalf("parse udp-relay.json: %v", err)
	}
	if vf.Schema != 1 {
		t.Fatalf("unexpected udp-relay.json schema: got %d want 1", vf.Schema)
	}
	return vf
}

func TestProtocolVectors(t *testing.T) {
	vf := loadUdpRelayVectors(t)

	for _, v := range vf.Vectors {
		t.Run(v.Name, func(t *testing.T) {
			frame, err := base64.StdEncoding.DecodeString(v.FrameB64)
			if err != nil {
				t.Fatalf("decode frame_b64: %v", err)
			}

			if v.ExpectError {
				_, err := Decode(frame)
				if err == nil {
					t.Fatalf("expected error, got nil")
				}
				if v.ErrorContains != "" && !strings.Contains(err.Error(), v.ErrorContains) {
					t.Fatalf("error mismatch:\n got: %q\nwant substring: %q", err.Error(), v.ErrorContains)
				}
				return
			}

			payload, err := base64.StdEncoding.DecodeString(v.PayloadB64)
			if err != nil {
				t.Fatalf("decode payload_b64: %v", err)
			}

			got, err := Decode(frame)
			if err != nil {
				t.Fatalf("Decode: %v", err)
			}
			if got.Version != v.Version {
				t.Fatalf("Version: got %d want %d", got.Version, v.Version)
			}
			if got.GuestPort != v.GuestPort {
				t.Fatalf("GuestPort: got %d want %d", got.GuestPort, v.GuestPort)
			}
			if got.RemotePort != v.RemotePort {
				t.Fatalf("RemotePort: got %d want %d", got.RemotePort, v.RemotePort)
			}

			wantIP := netip.MustParseAddr(v.RemoteIP)
			if got.RemoteIP != wantIP {
				t.Fatalf("RemoteIP: got %s want %s", got.RemoteIP, wantIP)
			}
			if !bytes.Equal(got.Payload, payload) {
				t.Fatalf("Payload: got %x want %x", got.Payload, payload)
			}

			roundTrip := Frame{
				Version:    v.Version,
				GuestPort:  v.GuestPort,
				RemoteIP:   wantIP,
				RemotePort: v.RemotePort,
				Payload:    payload,
			}

			var encoded []byte
			switch v.Version {
			case 1:
				encoded, err = EncodeV1(roundTrip)
			case 2:
				encoded, err = EncodeV2(roundTrip)
			default:
				t.Fatalf("unsupported vector version %d", v.Version)
			}
			if err != nil {
				t.Fatalf("encode: %v", err)
			}
			if !bytes.Equal(encoded, frame) {
				t.Fatalf("encoded frame mismatch:\n got: %x\nwant: %x", encoded, frame)
			}
		})
	}
}

