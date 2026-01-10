package signaling

import (
	"encoding/json"
	"testing"
)

func TestSignalMessage_MarshalUnmarshalOffer(t *testing.T) {
	msg := SignalMessage{
		Type: MessageTypeOffer,
		SDP: &SDP{
			Type: "offer",
			SDP:  "v=0",
		},
	}

	b, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	got, err := ParseSignalMessage(b)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}

	if got.Type != MessageTypeOffer || got.SDP == nil || got.SDP.Type != "offer" || got.SDP.SDP != "v=0" {
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

	got, err := ParseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != MessageTypeCandidate || got.Candidate == nil || got.Candidate.Candidate == "" {
		t.Fatalf("unexpected decoded candidate: %#v", got)
	}
}

func TestSignalMessage_DisallowUnknownFields(t *testing.T) {
	raw := []byte(`{ "type":"close", "unexpected": true }`)
	if _, err := ParseSignalMessage(raw); err == nil {
		t.Fatalf("expected error")
	}
}
