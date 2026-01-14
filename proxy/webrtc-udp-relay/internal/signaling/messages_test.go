package signaling

import (
	"encoding/json"
	"testing"
)

func TestSignalMessage_MarshalUnmarshalOffer(t *testing.T) {
	msg := signalMessage{
		Type: messageTypeOffer,
		SDP: &sdp{
			Type: "offer",
			SDP:  "v=0",
		},
	}

	b, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	got, err := parseSignalMessage(b)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}

	if got.Type != messageTypeOffer || got.SDP == nil || got.SDP.Type != "offer" || got.SDP.SDP != "v=0" {
		t.Fatalf("unexpected decoded offer: %#v", got)
	}
}

func TestSignalMessage_UnmarshalCandidate(t *testing.T) {
	raw := []byte(`{
		"type":"candidate",
		"candidate":{
			"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host",
			"sdpMid":"0",
			"sdpMLineIndex":0
		}
	}`)

	got, err := parseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != messageTypeCandidate || got.Candidate == nil || got.Candidate.Candidate == "" {
		t.Fatalf("unexpected decoded candidate: %#v", got)
	}
}

func TestSignalMessage_DisallowUnknownFields(t *testing.T) {
	raw := []byte(`{ "type":"close", "unexpected": true }`)
	if _, err := parseSignalMessage(raw); err == nil {
		t.Fatalf("expected error")
	}
}

func TestSignalMessage_UnmarshalAuth_AllowsTokenAndAPIKeyWhenMatching(t *testing.T) {
	raw := []byte(`{ "type":"auth", "token":"secret", "apiKey":"secret" }`)
	got, err := parseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != messageTypeAuth {
		t.Fatalf("type=%q, want %q", got.Type, messageTypeAuth)
	}
	if got.Token != "secret" || got.APIKey != "secret" {
		t.Fatalf("unexpected credential fields: %#v", got)
	}
}

func TestSignalMessage_UnmarshalAuth_RejectsTokenAndAPIKeyMismatch(t *testing.T) {
	raw := []byte(`{ "type":"auth", "token":"t1", "apiKey":"t2" }`)
	if _, err := parseSignalMessage(raw); err == nil {
		t.Fatalf("expected error")
	}
}
