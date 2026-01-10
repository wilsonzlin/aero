package config

import "testing"

func TestDefaultsDev(t *testing.T) {
	cfg, err := load(func(string) (string, bool) { return "", false }, nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Mode != ModeDev {
		t.Fatalf("mode=%q, want %q", cfg.Mode, ModeDev)
	}
	if cfg.LogFormat != LogFormatText {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatText)
	}
}

func TestDefaultsProdWhenModeFlagSet(t *testing.T) {
	cfg, err := load(func(string) (string, bool) { return "", false }, []string{"--mode", "prod"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Mode != ModeProd {
		t.Fatalf("mode=%q, want %q", cfg.Mode, ModeProd)
	}
	if cfg.LogFormat != LogFormatJSON {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatJSON)
	}
}

func TestLogFormatExplicitOverride(t *testing.T) {
	cfg, err := load(func(string) (string, bool) { return "", false }, []string{"--mode", "prod", "--log-format", "text"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.LogFormat != LogFormatText {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatText)
	}
}
