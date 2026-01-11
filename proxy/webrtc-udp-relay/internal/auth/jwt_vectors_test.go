package auth

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

type jwtVectorsFile struct {
	Version  int `json:"version"`
	RelayJWT struct {
		Secret  string `json:"secret"`
		NowUnix int64  `json:"nowUnix"`
		Tokens  struct {
			Valid struct {
				Token string `json:"token"`
			} `json:"valid"`
			Expired struct {
				Token string `json:"token"`
			} `json:"expired"`
			BadSignature struct {
				Token string `json:"token"`
			} `json:"badSignature"`
		} `json:"tokens"`
	} `json:"aero-udp-relay-jwt-hs256"`
}

func loadJWTVectors(t *testing.T) jwtVectorsFile {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}
	path := filepath.Join(filepath.Dir(thisFile), "../../../..", "crates/conformance/test-vectors/aero-vectors-v1.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read vectors file: %v", err)
	}
	var vectors jwtVectorsFile
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parse vectors json: %v", err)
	}
	return vectors
}

func TestJWTVerifierVectors(t *testing.T) {
	vectors := loadJWTVectors(t)
	if vectors.Version != 1 {
		t.Fatalf("unexpected vectors version: %d", vectors.Version)
	}

	verifier := NewJWTVerifier(vectors.RelayJWT.Secret)
	verifier.now = func() time.Time {
		return time.Unix(vectors.RelayJWT.NowUnix, 0)
	}

	if err := verifier.Verify(vectors.RelayJWT.Tokens.Valid.Token); err != nil {
		t.Fatalf("valid token rejected: %v", err)
	}
	if err := verifier.Verify(vectors.RelayJWT.Tokens.Expired.Token); err == nil {
		t.Fatalf("expected expired token to be rejected")
	}
	if err := verifier.Verify(vectors.RelayJWT.Tokens.BadSignature.Token); err == nil {
		t.Fatalf("expected bad-signature token to be rejected")
	}
}

