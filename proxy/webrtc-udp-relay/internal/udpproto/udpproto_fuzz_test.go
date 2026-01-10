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

func FuzzDecodeFrame(f *testing.F) {
	f.Add([]byte{})
	f.Add([]byte{0, 0, 0, 0, 0, 0, 0, 0})
	f.Add([]byte{0x27, 0x10, 0xc0, 0x00, 0x02, 0x01, 0x00, 0x35, 0x61, 0x62, 0x63})
	f.Add([]byte{
		0xA2, 0x02, 0x06, 0x00,
		0xBE, 0xEF,
		0x20, 0x01, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
		0xCA, 0xFE,
		0x01, 0x02, 0x03,
	})

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
		f1, err1 := Decode(b)
		f2, err2 := Decode(b)

		c1, c2 := classify(err1), classify(err2)
		if c1 != c2 {
			t.Fatalf("unstable result: err1=%v err2=%v", err1, err2)
		}
		if c1 == "ok" {
			if f1.Version != f2.Version || f1.GuestPort != f2.GuestPort || f1.RemoteIP != f2.RemoteIP || f1.RemotePort != f2.RemotePort || !bytes.Equal(f1.Payload, f2.Payload) {
				t.Fatalf("unstable decode: f1=%#v f2=%#v", f1, f2)
			}
		}
	})
}
