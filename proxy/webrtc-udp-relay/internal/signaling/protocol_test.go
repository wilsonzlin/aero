package signaling

import (
	"bytes"
	"encoding/json"
	"errors"
	"testing"
)

func mustCompactJSON(t *testing.T, b []byte) []byte {
	t.Helper()
	var out bytes.Buffer
	if err := json.Compact(&out, b); err != nil {
		t.Fatalf("json.Compact: %v", err)
	}
	return out.Bytes()
}

func TestOfferRequestJSON(t *testing.T) {
	raw := []byte(`{"version":1,"offer":{"type":"offer","sdp":"v=0..."}}`)

	req, err := ParseOfferRequestJSON(raw)
	if err != nil {
		t.Fatalf("ParseOfferRequestJSON: %v", err)
	}
	if req.Version != Version1 {
		t.Fatalf("Version: got %d want %d", req.Version, Version1)
	}
	if req.Offer.Type != "offer" || req.Offer.SDP != "v=0..." {
		t.Fatalf("Offer: got %#v", req.Offer)
	}

	encoded, err := json.Marshal(req)
	if err != nil {
		t.Fatalf("json.Marshal: %v", err)
	}
	if !bytes.Equal(mustCompactJSON(t, encoded), mustCompactJSON(t, raw)) {
		t.Fatalf("marshal mismatch: got %s want %s", encoded, raw)
	}
}

func TestAnswerResponseJSON(t *testing.T) {
	raw := []byte(`{"version":1,"answer":{"type":"answer","sdp":"v=0..."}}`)

	resp, err := ParseAnswerResponseJSON(raw)
	if err != nil {
		t.Fatalf("ParseAnswerResponseJSON: %v", err)
	}
	if resp.Version != Version1 {
		t.Fatalf("Version: got %d want %d", resp.Version, Version1)
	}
	if resp.Answer.Type != "answer" || resp.Answer.SDP != "v=0..." {
		t.Fatalf("Answer: got %#v", resp.Answer)
	}
}

func TestSignalingValidation(t *testing.T) {
	_, err := ParseOfferRequestJSON([]byte(`{"version":2,"offer":{"type":"offer","sdp":"x"}}`))
	if !errors.Is(err, ErrUnsupportedVersion) {
		t.Fatalf("unsupported version: got %v", err)
	}

	_, err = ParseOfferRequestJSON([]byte(`{"version":1,"offer":{"type":"answer","sdp":"x"}}`))
	if !errors.Is(err, ErrInvalidSDPType) {
		t.Fatalf("invalid type: got %v", err)
	}

	_, err = ParseOfferRequestJSON([]byte(`{"version":1,"offer":{"type":"offer","sdp":""}}`))
	if !errors.Is(err, ErrMissingSDP) {
		t.Fatalf("missing sdp: got %v", err)
	}
}
