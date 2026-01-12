package l2tunnel

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

type l2ValidVector struct {
	Name       string `json:"name"`
	MsgType    byte   `json:"msgType"`
	Flags      byte   `json:"flags"`
	PayloadHex string `json:"payloadHex"`
	WireHex    string `json:"wireHex"`
	Structured *struct {
		Code    uint16 `json:"code"`
		Message string `json:"message"`
	} `json:"structured"`
}

type l2InvalidVector struct {
	Name      string `json:"name"`
	WireHex   string `json:"wireHex"`
	ErrorCode string `json:"errorCode"`
}

type rootVectors struct {
	Version int `json:"version"`
	L2      struct {
		Valid   []l2ValidVector   `json:"valid"`
		Invalid []l2InvalidVector `json:"invalid"`
	} `json:"aero-l2-tunnel-v1"`
}

func mustDecodeHex(t *testing.T, raw string) []byte {
	t.Helper()
	b, err := hex.DecodeString(raw)
	if err != nil {
		t.Fatalf("decode hex %q: %v", raw, err)
	}
	return b
}

func loadVectors(t *testing.T) rootVectors {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}

	path := filepath.Join(filepath.Dir(thisFile), "../../../../crates/conformance/test-vectors/aero-vectors-v1.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read vectors: %v", err)
	}

	var out rootVectors
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatalf("unmarshal vectors: %v", err)
	}
	return out
}

func TestL2TunnelVectors(t *testing.T) {
	vectors := loadVectors(t)
	if vectors.Version != 1 {
		t.Fatalf("unexpected vectors version: %d", vectors.Version)
	}

	for _, v := range vectors.L2.Valid {
		v := v
		t.Run("valid/"+v.Name, func(t *testing.T) {
			payload := mustDecodeHex(t, v.PayloadHex)
			wire := mustDecodeHex(t, v.WireHex)

			msg, err := DecodeMessage(wire)
			if err != nil {
				t.Fatalf("DecodeMessage: %v", err)
			}
			if msg.Version != Version {
				t.Fatalf("Version=%#x, want %#x", msg.Version, Version)
			}
			if msg.Type != v.MsgType {
				t.Fatalf("Type=%#x, want %#x", msg.Type, v.MsgType)
			}
			if msg.Flags != v.Flags {
				t.Fatalf("Flags=%#x, want %#x", msg.Flags, v.Flags)
			}
			if !bytes.Equal(msg.Payload, payload) {
				t.Fatalf("payload mismatch: got %x, want %x", msg.Payload, payload)
			}

			if v.Structured != nil {
				code, message, ok := DecodeStructuredErrorPayload(msg.Payload)
				if !ok {
					t.Fatalf("DecodeStructuredErrorPayload returned ok=false")
				}
				if code != v.Structured.Code {
					t.Fatalf("structured code=%d, want %d", code, v.Structured.Code)
				}
				if message != v.Structured.Message {
					t.Fatalf("structured message=%q, want %q", message, v.Structured.Message)
				}
				encoded := EncodeStructuredErrorPayload(code, message, int(^uint(0)>>1))
				if !bytes.Equal(encoded, payload) {
					t.Fatalf("structured payload mismatch: got %x, want %x", encoded, payload)
				}
			}

			var encoded []byte
			switch msg.Type {
			case TypeFrame:
				encoded, err = EncodeFrame(payload)
			case TypePing:
				encoded, err = EncodePing(payload)
			case TypePong:
				encoded, err = EncodePong(payload)
			case TypeError:
				encoded, err = EncodeErrorMessage(payload)
			default:
				t.Fatalf("unsupported msg type in vectors: %#x", msg.Type)
			}
			if err != nil {
				t.Fatalf("encode: %v", err)
			}
			if got := hex.EncodeToString(encoded); got != v.WireHex {
				t.Fatalf("wire mismatch: got %s, want %s", got, v.WireHex)
			}
		})
	}

	for _, v := range vectors.L2.Invalid {
		v := v
		t.Run("invalid/"+v.Name, func(t *testing.T) {
			wire := mustDecodeHex(t, v.WireHex)
			_, err := DecodeMessage(wire)
			if err == nil {
				t.Fatalf("expected DecodeMessage to fail")
			}
			de, ok := err.(*DecodeError)
			if !ok {
				t.Fatalf("expected *DecodeError, got %T (%v)", err, err)
			}
			if string(de.Code) != v.ErrorCode {
				t.Fatalf("DecodeError.Code=%q, want %q", de.Code, v.ErrorCode)
			}
		})
	}
}

func TestEncodeStructuredErrorPayload_Truncation(t *testing.T) {
	// maxPayloadBytes=4 => header only.
	p := EncodeStructuredErrorPayload(123, "hello", 4)
	if len(p) != 4 {
		t.Fatalf("len=%d, want 4", len(p))
	}
	code, msg, ok := DecodeStructuredErrorPayload(p)
	if !ok {
		t.Fatalf("DecodeStructuredErrorPayload returned ok=false")
	}
	if code != 123 {
		t.Fatalf("code=%d, want 123", code)
	}
	if msg != "" {
		t.Fatalf("msg=%q, want empty", msg)
	}
}
