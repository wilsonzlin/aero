package origin

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

type originVectorsFile struct {
	Schema    int               `json:"schema"`
	Normalize []normalizeVector `json:"normalize"`
	Allow     []allowVector     `json:"allow"`
}

type normalizeVector struct {
	Name            string `json:"name"`
	RawOriginHeader string `json:"rawOriginHeader"`

	NormalizedOrigin string `json:"normalizedOrigin"`
	ExpectError      bool   `json:"expectError"`
}

type allowVector struct {
	Name            string   `json:"name"`
	AllowedOrigins  []string `json:"allowedOrigins"`
	RequestHost     string   `json:"requestHost"`
	RawOriginHeader string   `json:"rawOriginHeader"`
	ExpectAllowed   bool     `json:"expectAllowed"`
}

func findRepoRoot(startDir string) (string, error) {
	dir := startDir
	for {
		// Heuristics: repo root always has AGENTS.md, and also currently has a Cargo.toml.
		for _, marker := range []string{"AGENTS.md", "Cargo.toml"} {
			if _, err := os.Stat(filepath.Join(dir, marker)); err == nil {
				return dir, nil
			}
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			return "", fmt.Errorf("repo root not found from %s", startDir)
		}
		dir = parent
	}
}

func loadOriginVectors(t *testing.T) originVectorsFile {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}

	root, err := findRepoRoot(filepath.Dir(thisFile))
	if err != nil {
		t.Fatalf("find repo root: %v", err)
	}

	b, err := os.ReadFile(filepath.Join(root, "protocol-vectors", "origin.json"))
	if err != nil {
		t.Fatalf("read origin.json: %v", err)
	}

	var vf originVectorsFile
	if err := json.Unmarshal(b, &vf); err != nil {
		t.Fatalf("parse origin.json: %v", err)
	}
	if vf.Schema != 1 {
		t.Fatalf("unexpected origin.json schema: got %d want 1", vf.Schema)
	}
	return vf
}

func TestOriginVectors(t *testing.T) {
	vf := loadOriginVectors(t)

	for _, v := range vf.Normalize {
		t.Run("normalize/"+v.Name, func(t *testing.T) {
			normalized, _, ok := NormalizeHeader(v.RawOriginHeader)
			if v.ExpectError {
				if ok {
					t.Fatalf("expected ok=false, got ok=true (normalized=%q)", normalized)
				}
				return
			}
			if !ok {
				t.Fatalf("expected ok=true, got ok=false")
			}
			if normalized != v.NormalizedOrigin {
				t.Fatalf("normalized=%q, want %q", normalized, v.NormalizedOrigin)
			}
		})
	}

	for _, v := range vf.Allow {
		t.Run("allow/"+v.Name, func(t *testing.T) {
			normalized, host, ok := NormalizeHeader(v.RawOriginHeader)
			allowed := false
			if ok {
				allowed = IsAllowed(normalized, host, v.RequestHost, v.AllowedOrigins)
			}
			if allowed != v.ExpectAllowed {
				t.Fatalf("allowed=%v, want %v", allowed, v.ExpectAllowed)
			}
		})
	}
}
