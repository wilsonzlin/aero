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

