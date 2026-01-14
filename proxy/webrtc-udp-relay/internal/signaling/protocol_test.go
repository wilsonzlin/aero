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

	var req offerRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}
	if err := req.Validate(); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if req.Version != version1 {
		t.Fatalf("Version: got %d want %d", req.Version, version1)
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

	var resp answerResponse
	if err := json.Unmarshal(raw, &resp); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}
	if err := resp.Validate(); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if resp.Version != version1 {
		t.Fatalf("Version: got %d want %d", resp.Version, version1)
	}
	if resp.Answer.Type != "answer" || resp.Answer.SDP != "v=0..." {
		t.Fatalf("Answer: got %#v", resp.Answer)
	}
}

func TestSignalingValidation(t *testing.T) {
	var req offerRequest
	if err := json.Unmarshal([]byte(`{"version":2,"offer":{"type":"offer","sdp":"x"}}`), &req); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}
	if err := req.Validate(); !errors.Is(err, errUnsupportedVersion) {
		t.Fatalf("unsupported version: got %v", err)
	}

	if err := json.Unmarshal([]byte(`{"version":1,"offer":{"type":"answer","sdp":"x"}}`), &req); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}
	if err := req.Validate(); !errors.Is(err, errInvalidSDPType) {
		t.Fatalf("invalid type: got %v", err)
	}

	if err := json.Unmarshal([]byte(`{"version":1,"offer":{"type":"offer","sdp":""}}`), &req); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}
	if err := req.Validate(); !errors.Is(err, errMissingSDP) {
		t.Fatalf("missing sdp: got %v", err)
	}
}
