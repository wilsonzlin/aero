package config

import (
	"flag"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"time"
)

const (
	EnvListenAddr           = "AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR"
	EnvPublicBaseURL        = "AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL"
	EnvLogFormat            = "AERO_WEBRTC_UDP_RELAY_LOG_FORMAT"
	EnvLogLevel             = "AERO_WEBRTC_UDP_RELAY_LOG_LEVEL"
	EnvShutdownTimeout      = "AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT"
	EnvMode                 = "AERO_WEBRTC_UDP_RELAY_MODE"
	DefaultListenAddr       = "127.0.0.1:8080"
	DefaultShutdown         = 15 * time.Second
	DefaultMode        Mode = ModeDev
)

type Mode string

const (
	ModeDev  Mode = "dev"
	ModeProd Mode = "prod"
)

type LogFormat string

const (
	LogFormatText LogFormat = "text"
	LogFormatJSON LogFormat = "json"
)

type Config struct {
	ListenAddr      string
	PublicBaseURL   string
	LogFormat       LogFormat
	LogLevel        slog.Level
	ShutdownTimeout time.Duration
	Mode            Mode
}

func Load(args []string) (Config, error) {
	return load(os.LookupEnv, args)
}

func load(lookup func(string) (string, bool), args []string) (Config, error) {
	envMode, _ := lookup(EnvMode)
	modeDefault := string(DefaultMode)
	if envMode != "" {
		modeDefault = envMode
	}

	envLogFormat, envLogFormatOK := lookup(EnvLogFormat)
	envLogFormatSet := envLogFormatOK && envLogFormat != ""
	logFormatDefault := envLogFormat
	if !envLogFormatSet {
		logFormatDefault = defaultLogFormatForMode(modeDefault)
	}

	envLogLevel, envLogLevelOK := lookup(EnvLogLevel)
	envLogLevelSet := envLogLevelOK && envLogLevel != ""
	logLevelDefault := envLogLevel
	if !envLogLevelSet {
		logLevelDefault = defaultLogLevelForMode(modeDefault)
	}

	listenAddr := envOrDefault(lookup, EnvListenAddr, DefaultListenAddr)
	publicBaseURL := envOrDefault(lookup, EnvPublicBaseURL, "")

	shutdownTimeout := DefaultShutdown
	if raw, ok := lookup(EnvShutdownTimeout); ok && raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", EnvShutdownTimeout, raw, err)
		}
		shutdownTimeout = d
	}

	fs := flag.NewFlagSet("aero-webrtc-udp-relay", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)

	var (
		modeStr      string
		logFormatStr string
		logLevelStr  string
	)

	fs.StringVar(&listenAddr, "listen-addr", listenAddr, "HTTP listen address (host:port)")
	fs.StringVar(&publicBaseURL, "public-base-url", publicBaseURL, "Public base URL (optional; used for logging)")
	fs.StringVar(&modeStr, "mode", modeDefault, "Run mode: dev or prod")
	fs.StringVar(&logFormatStr, "log-format", logFormatDefault, "Log format: text or json")
	fs.StringVar(&logLevelStr, "log-level", logLevelDefault, "Log level: debug, info, warn, error")
	fs.DurationVar(&shutdownTimeout, "shutdown-timeout", shutdownTimeout, "Graceful shutdown timeout (e.g. 15s)")

	if err := fs.Parse(args); err != nil {
		return Config{}, err
	}

	setFlags := map[string]bool{}
	fs.Visit(func(f *flag.Flag) {
		setFlags[f.Name] = true
	})

	mode, err := parseMode(modeStr)
	if err != nil {
		return Config{}, err
	}

	if !envLogFormatSet && !setFlags["log-format"] {
		logFormatStr = defaultLogFormatForMode(string(mode))
	}
	if !envLogLevelSet && !setFlags["log-level"] {
		logLevelStr = defaultLogLevelForMode(string(mode))
	}

	logFormat, err := parseLogFormat(logFormatStr)
	if err != nil {
		return Config{}, err
	}

	level, err := parseLogLevel(logLevelStr)
	if err != nil {
		return Config{}, err
	}

	if listenAddr == "" {
		return Config{}, fmt.Errorf("listen address must not be empty")
	}
	if shutdownTimeout <= 0 {
		return Config{}, fmt.Errorf("shutdown timeout must be > 0")
	}

	return Config{
		ListenAddr:      listenAddr,
		PublicBaseURL:   publicBaseURL,
		LogFormat:       logFormat,
		LogLevel:        level,
		ShutdownTimeout: shutdownTimeout,
		Mode:            mode,
	}, nil
}

func NewLogger(cfg Config) (*slog.Logger, error) {
	opts := &slog.HandlerOptions{
		Level: cfg.LogLevel,
	}

	var handler slog.Handler
	switch cfg.LogFormat {
	case LogFormatText:
		handler = slog.NewTextHandler(os.Stdout, opts)
	case LogFormatJSON:
		handler = slog.NewJSONHandler(os.Stdout, opts)
	default:
		return nil, fmt.Errorf("unsupported log format %q", cfg.LogFormat)
	}

	return slog.New(handler), nil
}

func envOrDefault(lookup func(string) (string, bool), key, fallback string) string {
	if v, ok := lookup(key); ok && v != "" {
		return v
	}
	return fallback
}

func defaultLogFormatForMode(mode string) string {
	switch strings.ToLower(strings.TrimSpace(mode)) {
	case string(ModeProd), "production":
		return string(LogFormatJSON)
	default:
		return string(LogFormatText)
	}
}

func defaultLogLevelForMode(mode string) string {
	switch strings.ToLower(strings.TrimSpace(mode)) {
	case string(ModeProd), "production":
		return "info"
	default:
		return "debug"
	}
}

func parseMode(raw string) (Mode, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case string(ModeDev), "development":
		return ModeDev, nil
	case string(ModeProd), "production":
		return ModeProd, nil
	default:
		return "", fmt.Errorf("invalid mode %q (expected dev or prod)", raw)
	}
}

func parseLogFormat(raw string) (LogFormat, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case string(LogFormatText):
		return LogFormatText, nil
	case string(LogFormatJSON):
		return LogFormatJSON, nil
	default:
		return "", fmt.Errorf("invalid log format %q (expected text or json)", raw)
	}
}

func parseLogLevel(raw string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "debug":
		return slog.LevelDebug, nil
	case "info":
		return slog.LevelInfo, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	default:
		return slog.LevelInfo, fmt.Errorf("invalid log level %q (expected debug, info, warn, error)", raw)
	}
}
