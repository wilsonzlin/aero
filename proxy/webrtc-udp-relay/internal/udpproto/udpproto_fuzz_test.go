package udpproto

import (
	"bytes"
	"errors"
	"testing"
)

func FuzzDecodeDatagram(f *testing.F) {
	f.Add([]byte{})
	f.Add([]byte{0, 0, 0, 0, 0, 0, 0, 0})
	f.Add([]byte{0x27, 0x10, 0xc0, 0x00, 0x02, 0x01, 0x00, 0x35, 0x61, 0x62, 0x63})

	classify := func(err error) string {
		switch {
		case err == nil:
			return "ok"
		case errors.Is(err, ErrTooShort):
			return "too_short"
		case errors.Is(err, ErrPayloadTooLarge):
			return "payload_too_large"
		default:
			return "other"
		}
	}

	f.Fuzz(func(t *testing.T, b []byte) {
		d1, err1 := DecodeDatagram(b)
		d2, err2 := DecodeDatagram(b)

		c1, c2 := classify(err1), classify(err2)
		if c1 == "other" || c2 == "other" {
			t.Fatalf("unexpected error types: err1=%v err2=%v", err1, err2)
		}
		if c1 != c2 {
			t.Fatalf("unstable result: err1=%v err2=%v", err1, err2)
		}
		if c1 == "ok" {
			if d1.GuestPort != d2.GuestPort || d1.RemoteIP != d2.RemoteIP || d1.RemotePort != d2.RemotePort || !bytes.Equal(d1.Payload, d2.Payload) {
				t.Fatalf("unstable decode: d1=%#v d2=%#v", d1, d2)
			}
		}
	})
}
