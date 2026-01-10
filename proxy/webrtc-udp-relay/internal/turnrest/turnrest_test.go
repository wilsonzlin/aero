package turnrest

import (
	"crypto/hmac"
	"crypto/sha1"
	"encoding/base64"
	"testing"
	"time"
)

func TestGenerate_DeterministicWithFixedTime(t *testing.T) {
	g, err := NewGenerator(GeneratorConfig{
		SharedSecret:    "shared-secret",
		TTLSeconds:      3600,
		UsernamePrefix:  "aero",
		Now:             func() time.Time { return time.Unix(1_700_000_000, 0).UTC() },
		SessionIDSource: func() (string, error) { return "unused", nil },
	})
	if err != nil {
		t.Fatalf("NewGenerator: %v", err)
	}

	creds, err := g.Generate("session123")
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}

	wantExpiry := int64(1_700_003_600)
	if creds.ExpiryUnix != wantExpiry {
		t.Fatalf("ExpiryUnix: got %d, want %d", creds.ExpiryUnix, wantExpiry)
	}
	wantUsername := "1700003600:aero:session123"
	if creds.Username != wantUsername {
		t.Fatalf("Username: got %q, want %q", creds.Username, wantUsername)
	}

	wantCred := expectedCredential(t, []byte("shared-secret"), wantUsername)
	if creds.Credential != wantCred {
		t.Fatalf("Credential: got %q, want %q", creds.Credential, wantCred)
	}
}

func TestGenerate_TTLBehavior(t *testing.T) {
	now := time.Unix(42, 0).UTC()
	g, err := NewGenerator(GeneratorConfig{
		SharedSecret:    "secret",
		TTLSeconds:      10,
		UsernamePrefix:  "aero",
		Now:             func() time.Time { return now },
		SessionIDSource: func() (string, error) { return "unused", nil },
	})
	if err != nil {
		t.Fatalf("NewGenerator: %v", err)
	}

	creds, err := g.Generate("abc")
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}
	if creds.ExpiryUnix != now.Unix()+10 {
		t.Fatalf("ExpiryUnix: got %d, want %d", creds.ExpiryUnix, now.Unix()+10)
	}
}

func TestGenerate_CredentialBase64AndHMACSHA1(t *testing.T) {
	g, err := NewGenerator(GeneratorConfig{
		SharedSecret:    "secret",
		TTLSeconds:      1,
		UsernamePrefix:  "pfx",
		Now:             func() time.Time { return time.Unix(0, 0).UTC() },
		SessionIDSource: func() (string, error) { return "unused", nil },
	})
	if err != nil {
		t.Fatalf("NewGenerator: %v", err)
	}

	creds, err := g.Generate("sid")
	if err != nil {
		t.Fatalf("Generate: %v", err)
	}

	decoded, err := base64.StdEncoding.DecodeString(creds.Credential)
	if err != nil {
		t.Fatalf("DecodeString: %v", err)
	}
	if len(decoded) != sha1.Size {
		t.Fatalf("decoded length: got %d, want %d", len(decoded), sha1.Size)
	}

	mac := hmac.New(sha1.New, []byte("secret"))
	_, _ = mac.Write([]byte(creds.Username))
	want := mac.Sum(nil)
	if string(decoded) != string(want) {
		t.Fatalf("decoded HMAC mismatch")
	}
}

func expectedCredential(t *testing.T, sharedSecret []byte, username string) string {
	t.Helper()
	mac := hmac.New(sha1.New, sharedSecret)
	_, _ = mac.Write([]byte(username))
	return base64.StdEncoding.EncodeToString(mac.Sum(nil))
}
