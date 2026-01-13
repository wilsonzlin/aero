package webrtcpeer

import (
	"net/url"
	"strings"
	"testing"
)

func TestSanitizeWSURLForLog_StripsSensitiveParts(t *testing.T) {
	raw := "wss://user:pass@example.com/l2?token=sekrit#frag"
	got := sanitizeWSURLForLog(raw)
	want := "wss://example.com/l2"
	if got != want {
		t.Fatalf("sanitizeWSURLForLog(%q)=%q, want %q", raw, got, want)
	}
}

func TestSanitizeWSURLForLog_StripsUserInfoOnParseFailure(t *testing.T) {
	// Include an invalid URL escape to force url.Parse to fail while still
	// containing userinfo.
	raw := "wss://user:pass@example.com/l2?token=%ZZ"
	got := sanitizeWSURLForLog(raw)
	want := "wss://example.com/l2"
	if got != want {
		t.Fatalf("sanitizeWSURLForLog(%q)=%q, want %q", raw, got, want)
	}
}

func TestL2Bridge_SanitizeStringForLog_RedactsCredentialAndEscapedCredential(t *testing.T) {
	cred := "bad=secret"
	b := &l2Bridge{
		dialCfg: l2BackendDialConfig{
			Credential: cred,
		},
	}

	msg := "dial ws://example.com/l2?token=" + url.QueryEscape(cred) + ": 403"
	s := b.sanitizeStringForLog(msg)

	if strings.Contains(s, cred) {
		t.Fatalf("sanitized message still contains credential %q: %q", cred, s)
	}
	if strings.Contains(s, url.QueryEscape(cred)) {
		t.Fatalf("sanitized message still contains escaped credential %q: %q", url.QueryEscape(cred), s)
	}
	if !strings.Contains(s, "<redacted>") {
		t.Fatalf("expected sanitized message to contain <redacted>: %q", s)
	}
}

func TestL2Bridge_SanitizeStringForLog_RedactsQueryTokenEvenWhenValueUnknown(t *testing.T) {
	b := &l2Bridge{}
	msg := "dial ws://example.com/l2?token=sekrit&foo=bar: 403"
	s := b.sanitizeStringForLog(msg)
	if strings.Contains(s, "sekrit") {
		t.Fatalf("sanitized message still contains query token value: %q", s)
	}
	if !strings.Contains(s, "token=<redacted>") {
		t.Fatalf("expected token query param to be redacted: %q", s)
	}
}

func TestL2Bridge_SanitizeStringForLog_RedactsAeroSessionCookieEvenWhenValueUnknown(t *testing.T) {
	b := &l2Bridge{}
	msg := "Cookie: aero_session=sess123; other=ok"
	s := b.sanitizeStringForLog(msg)
	if strings.Contains(s, "sess123") {
		t.Fatalf("sanitized message still contains aero_session cookie value: %q", s)
	}
	if !strings.Contains(s, "aero_session=<redacted>") {
		t.Fatalf("expected aero_session cookie value to be redacted: %q", s)
	}
}
