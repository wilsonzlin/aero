package signaling

import (
	"encoding/json"
	"reflect"
	"testing"
)

func FuzzParseSignalMessage(f *testing.F) {
	f.Add([]byte(`{"type":"offer","sdp":{"type":"offer","sdp":"v=0"}}`))
	f.Add([]byte(`{"type":"candidate","candidate":{"candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host","sdpMid":"0","sdpMLineIndex":0}}`))
	f.Add([]byte(`{"type":"auth","token":"secret","apiKey":"secret"}`))
	f.Add([]byte(`{"type":"close"}`))
	f.Add([]byte(`{"type":"error","code":"internal_error","message":"internal error"}`))

	// Known-bad cases from unit tests and common mistakes.
	f.Add([]byte(`{ "type":"close", "unexpected": true }`))
	f.Add([]byte(`{ "type":"auth", "token":"t1", "apiKey":"t2" }`))
	f.Add([]byte(`{"type":"bogus"}`))
	f.Add([]byte(`{"type":"close"}{"type":"close"}`))
	f.Add([]byte(`[]`))
	f.Add([]byte{})

	f.Fuzz(func(t *testing.T, data []byte) {
		msg1, err1 := parseSignalMessage(data)
		msg2, err2 := parseSignalMessage(data)
		if (err1 == nil) != (err2 == nil) {
			t.Fatalf("non-deterministic parse result: err1=%v err2=%v", err1, err2)
		}
		if err1 != nil {
			return
		}

		// Successful parses must always produce a message that validates.
		if err := msg1.validate(); err != nil {
			t.Fatalf("validate() failed after successful parse: %v", err)
		}

		// Parsing should be stable for identical inputs.
		if !reflect.DeepEqual(msg1, msg2) {
			t.Fatalf("non-deterministic parse output: msg1=%#v msg2=%#v", msg1, msg2)
		}

		// Round-trip through JSON should preserve semantics and remain strict.
		b, err := json.Marshal(msg1)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		round, err := parseSignalMessage(b)
		if err != nil {
			t.Fatalf("re-parse marshaled message: %v (json=%q)", err, string(b))
		}
		if !reflect.DeepEqual(msg1, round) {
			t.Fatalf("round-trip mismatch: msg=%#v round=%#v json=%q", msg1, round, string(b))
		}
	})
}

func FuzzParseHTTPOfferRequest(f *testing.F) {
	// Accepted wire formats.
	f.Add([]byte(`{"sdp":{"type":"offer","sdp":"v=0"}}`))
	f.Add([]byte(`{"type":"offer","sdp":"v=0"}`))
	f.Add([]byte(`{"sdp":{"type":"answer","sdp":"v=0"}}`))

	// Known-bad cases.
	f.Add([]byte(`{"sdp":{"type":"offer","sdp":"v=0"}}{"type":"offer","sdp":"v=0"}`))
	f.Add([]byte(`{"type":"offer","sdp":"v=0","unexpected":true}`))
	f.Add([]byte(`{"sdp":{"type":"offer","sdp":"v=0","unexpected":true}}`))
	f.Add([]byte(`{"sdp":{}}`))
	f.Add([]byte{})

	f.Fuzz(func(t *testing.T, body []byte) {
		sdp1, err1 := parseHTTPOfferRequest(body)
		sdp2, err2 := parseHTTPOfferRequest(body)
		if (err1 == nil) != (err2 == nil) {
			t.Fatalf("non-deterministic parse result: err1=%v err2=%v", err1, err2)
		}
		if err1 != nil {
			return
		}
		if sdp1 != sdp2 {
			t.Fatalf("non-deterministic parse output: sdp1=%#v sdp2=%#v", sdp1, sdp2)
		}

		// parseHTTPOfferRequest only succeeds when decodeStrictJSON succeeds,
		// which implies the input is a single valid JSON value.
		if !json.Valid(body) {
			t.Fatalf("parse succeeded but json.Valid returned false")
		}

		// Ensure downstream SDP conversions are also panic-safe.
		_, _ = sdp1.ToPion()

		// The parsed SDP should be representable as strict JSON and round-trip.
		b, err := json.Marshal(sdp1)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		var round sdp
		if err := decodeStrictJSON(b, &round); err != nil {
			t.Fatalf("decodeStrictJSON(marshal(sdp)) failed: %v (json=%q)", err, string(b))
		}
		if round != sdp1 {
			t.Fatalf("round-trip mismatch: sdp=%#v round=%#v json=%q", sdp1, round, string(b))
		}
	})
}

func FuzzDecodeStrictJSON(f *testing.F) {
	f.Add([]byte(`{"type":"offer","sdp":"v=0"}`), uint8(0))
	f.Add([]byte(`{"sdp":{"type":"offer","sdp":"v=0"}}`), uint8(1))
	f.Add([]byte(`{"type":"offer","sdp":"v=0"}{"type":"offer","sdp":"v=0"}`), uint8(0))
	f.Add([]byte(`[]`), uint8(1))
	f.Add([]byte{}, uint8(0))

	f.Fuzz(func(t *testing.T, body []byte, which uint8) {
		switch which % 2 {
		case 0:
			var v1, v2 sdp
			err1 := decodeStrictJSON(body, &v1)
			err2 := decodeStrictJSON(body, &v2)
			if (err1 == nil) != (err2 == nil) {
				t.Fatalf("non-deterministic result: err1=%v err2=%v", err1, err2)
			}
			if err1 != nil {
				return
			}
			if v1 != v2 {
				t.Fatalf("non-deterministic output: v1=%#v v2=%#v", v1, v2)
			}
			if !json.Valid(body) {
				t.Fatalf("decodeStrictJSON succeeded but json.Valid returned false")
			}

			roundBytes, err := json.Marshal(v1)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			var round sdp
			if err := decodeStrictJSON(roundBytes, &round); err != nil {
				t.Fatalf("decodeStrictJSON(marshal(v)) failed: %v (json=%q)", err, string(roundBytes))
			}
			if round != v1 {
				t.Fatalf("round-trip mismatch: v=%#v round=%#v json=%q", v1, round, string(roundBytes))
			}

		case 1:
			var v1, v2 httpOfferRequest
			err1 := decodeStrictJSON(body, &v1)
			err2 := decodeStrictJSON(body, &v2)
			if (err1 == nil) != (err2 == nil) {
				t.Fatalf("non-deterministic result: err1=%v err2=%v", err1, err2)
			}
			if err1 != nil {
				return
			}
			if v1 != v2 {
				t.Fatalf("non-deterministic output: v1=%#v v2=%#v", v1, v2)
			}
			if !json.Valid(body) {
				t.Fatalf("decodeStrictJSON succeeded but json.Valid returned false")
			}
		}
	})
}
