package config

import (
	"flag"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/pion/webrtc/v4"
)

const (
	EnvListenAddr      = "AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR"
	EnvPublicBaseURL   = "AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL"
	EnvAllowedOrigins  = "ALLOWED_ORIGINS"
	EnvLogFormat       = "AERO_WEBRTC_UDP_RELAY_LOG_FORMAT"
	EnvLogLevel        = "AERO_WEBRTC_UDP_RELAY_LOG_LEVEL"
	EnvShutdownTimeout = "AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT"
	EnvMode            = "AERO_WEBRTC_UDP_RELAY_MODE"

	// Quota/rate limiting knobs (required by the task).
	EnvMaxSessions                     = "MAX_SESSIONS"
	EnvMaxUDPPpsPerSession             = "MAX_UDP_PPS_PER_SESSION"
	EnvMaxUDPBpsPerSession             = "MAX_UDP_BPS_PER_SESSION"
	EnvMaxUDPPpsPerDest                = "MAX_UDP_PPS_PER_DEST"
	EnvMaxUDPBindingsPerSession        = "MAX_UDP_BINDINGS_PER_SESSION"
	EnvMaxUniqueDestinationsPerSession = "MAX_UNIQUE_DESTINATIONS_PER_SESSION"
	EnvMaxDataChannelBpsPerSession     = "MAX_DC_BPS_PER_SESSION"
	EnvHardCloseAfterViolations        = "HARD_CLOSE_AFTER_VIOLATIONS"
	EnvViolationWindowSeconds          = "VIOLATION_WINDOW_SECONDS"

	DefaultListenAddr           = "127.0.0.1:8080"
	DefaultShutdown             = 15 * time.Second
	DefaultViolationWindow      = 10 * time.Second
	DefaultMode            Mode = ModeDev
)

const (
	EnvWebRTCUDPPortMin = "WEBRTC_UDP_PORT_MIN"
	EnvWebRTCUDPPortMax = "WEBRTC_UDP_PORT_MAX"

	EnvWebRTCNAT1To1IPs             = "WEBRTC_NAT_1TO1_IPS"
	EnvWebRTCNAT1To1IPCandidateType = "WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE"

	EnvWebRTCUDPListenIP     = "WEBRTC_UDP_LISTEN_IP"
	DefaultWebRTCUDPListenIP = "0.0.0.0"
)

const (
	FlagWebRTCUDPPortMin = "webrtc-udp-port-min"
	FlagWebRTCUDPPortMax = "webrtc-udp-port-max"

	FlagWebRTCNAT1To1IPs             = "webrtc-nat-1to1-ips"
	FlagWebRTCNAT1To1IPCandidateType = "webrtc-nat-1to1-ip-candidate-type"

	FlagWebRTCUDPListenIP = "webrtc-udp-listen-ip"
)

// RecommendedWebRTCUDPPortRangeSize is an intentionally conservative minimum.
// Each WebRTC session may consume multiple UDP ports (depending on ICE
// settings), and running out of ports manifests as hard-to-debug connectivity
// failures.
const RecommendedWebRTCUDPPortRangeSize = 100

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

type NAT1To1IPCandidateType string

const (
	NAT1To1CandidateTypeHost  NAT1To1IPCandidateType = "host"
	NAT1To1CandidateTypeSrflx NAT1To1IPCandidateType = "srflx"
)

type UDPPortRange struct {
	Min uint16
	Max uint16
}

type Config struct {
	ListenAddr      string
	PublicBaseURL   string
	AllowedOrigins  []string
	LogFormat       LogFormat
	LogLevel        slog.Level
	ShutdownTimeout time.Duration
	Mode            Mode

	// WebRTCUDPPortRange restricts the UDP ports used for ICE. When nil, pion uses
	// its defaults (OS ephemeral port selection).
	WebRTCUDPPortRange *UDPPortRange

	// WebRTCNAT1To1IPs configures pion to advertise these public IPs for ICE when
	// the relay is behind NAT. Values must be literal IPs (no hostnames).
	WebRTCNAT1To1IPs []string

	// WebRTCNAT1To1IPCandidateType configures whether the NAT 1:1 IPs are
	// advertised as host or srflx ICE candidates.
	WebRTCNAT1To1IPCandidateType NAT1To1IPCandidateType

	// WebRTCUDPListenIP restricts which local interface address ICE will bind UDP
	// sockets to. 0.0.0.0 means "use library default" (typically all interfaces).
	WebRTCUDPListenIP net.IP

	// Quotas/rate limiting.
	//
	// A value <= 0 generally means "unlimited" / disabled.
	MaxSessions                     int
	MaxUDPPpsPerSession             int
	MaxUDPBpsPerSession             int
	MaxUDPPpsPerDest                int
	MaxUDPBindingsPerSession        int
	MaxUniqueDestinationsPerSession int
	MaxDataChannelBpsPerSession     int
	HardCloseAfterViolations        int
	ViolationWindow                 time.Duration
	ICEServers                      []webrtc.ICEServer

	iceConfigErr error
}

func (c Config) ICEConfigError() error {
	return c.iceConfigErr
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
	allowedOriginsStr := envOrDefault(lookup, EnvAllowedOrigins, "")
	iceServersJSON := envOrDefault(lookup, envICEServersJSON, "")
	stunURLs := envOrDefault(lookup, envStunURLs, "")
	turnURLs := envOrDefault(lookup, envTurnURLs, "")
	turnUsername := envOrDefault(lookup, envTurnUsername, "")
	turnCredential := envOrDefault(lookup, envTurnCredential, "")

	shutdownTimeout := DefaultShutdown
	if raw, ok := lookup(EnvShutdownTimeout); ok && raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", EnvShutdownTimeout, raw, err)
		}
		shutdownTimeout = d
	}

	maxSessions, err := envIntOrDefault(lookup, EnvMaxSessions, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPPpsPerSession, err := envIntOrDefault(lookup, EnvMaxUDPPpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPBpsPerSession, err := envIntOrDefault(lookup, EnvMaxUDPBpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPPpsPerDest, err := envIntOrDefault(lookup, EnvMaxUDPPpsPerDest, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPBindingsPerSession, err := envIntOrDefault(lookup, EnvMaxUDPBindingsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxUniqueDestinationsPerSession, err := envIntOrDefault(lookup, EnvMaxUniqueDestinationsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxDataChannelBpsPerSession, err := envIntOrDefault(lookup, EnvMaxDataChannelBpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	hardCloseAfterViolations, err := envIntOrDefault(lookup, EnvHardCloseAfterViolations, 0)
	if err != nil {
		return Config{}, err
	}

	violationWindow := DefaultViolationWindow
	if raw, ok := lookup(EnvViolationWindowSeconds); ok && strings.TrimSpace(raw) != "" {
		seconds, err := strconv.Atoi(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", EnvViolationWindowSeconds, raw, err)
		}
		if seconds > 0 {
			violationWindow = time.Duration(seconds) * time.Second
		}
	}

	// WebRTC network defaults (env values become flag defaults).
	var webrtcUDPPortMin uint
	if raw, ok := lookup(EnvWebRTCUDPPortMin); ok && strings.TrimSpace(raw) != "" {
		p, err := parsePortString(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", EnvWebRTCUDPPortMin, raw, err)
		}
		webrtcUDPPortMin = uint(p)
	}

	var webrtcUDPPortMax uint
	if raw, ok := lookup(EnvWebRTCUDPPortMax); ok && strings.TrimSpace(raw) != "" {
		p, err := parsePortString(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", EnvWebRTCUDPPortMax, raw, err)
		}
		webrtcUDPPortMax = uint(p)
	}

	if (webrtcUDPPortMin == 0) != (webrtcUDPPortMax == 0) {
		return Config{}, fmt.Errorf("%s and %s must be set together (or both unset)", EnvWebRTCUDPPortMin, EnvWebRTCUDPPortMax)
	}

	webrtcUDPListenIPStr := envOrDefault(lookup, EnvWebRTCUDPListenIP, DefaultWebRTCUDPListenIP)
	webrtcNAT1To1IPsStr := envOrDefault(lookup, EnvWebRTCNAT1To1IPs, "")
	webrtcNAT1To1CandidateTypeStr := envOrDefault(lookup, EnvWebRTCNAT1To1IPCandidateType, string(NAT1To1CandidateTypeHost))

	fs := flag.NewFlagSet("aero-webrtc-udp-relay", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)

	var (
		modeStr      string
		logFormatStr string
		logLevelStr  string
	)

	fs.StringVar(&listenAddr, "listen-addr", listenAddr, "HTTP listen address (host:port)")
	fs.StringVar(&publicBaseURL, "public-base-url", publicBaseURL, "Public base URL (optional; used for logging)")
	fs.StringVar(&allowedOriginsStr, "allowed-origins", allowedOriginsStr, "Comma-separated list of allowed browser origins (env "+EnvAllowedOrigins+")")
	fs.StringVar(&modeStr, "mode", modeDefault, "Run mode: dev or prod")
	fs.StringVar(&logFormatStr, "log-format", logFormatDefault, "Log format: text or json")
	fs.StringVar(&logLevelStr, "log-level", logLevelDefault, "Log level: debug, info, warn, error")
	fs.DurationVar(&shutdownTimeout, "shutdown-timeout", shutdownTimeout, "Graceful shutdown timeout (e.g. 15s)")
	fs.StringVar(&iceServersJSON, "ice-servers-json", iceServersJSON, "ICE server JSON config (AERO_ICE_SERVERS_JSON)")
	fs.StringVar(&stunURLs, "stun-urls", stunURLs, "comma-separated STUN URLs (AERO_STUN_URLS)")
	fs.StringVar(&turnURLs, "turn-urls", turnURLs, "comma-separated TURN URLs (AERO_TURN_URLS)")
	fs.StringVar(&turnUsername, "turn-username", turnUsername, "TURN username (AERO_TURN_USERNAME)")
	fs.StringVar(&turnCredential, "turn-credential", turnCredential, "TURN credential (AERO_TURN_CREDENTIAL)")

	fs.UintVar(&webrtcUDPPortMin, FlagWebRTCUDPPortMin, webrtcUDPPortMin, "Min UDP port for WebRTC ICE (0 = unset; env "+EnvWebRTCUDPPortMin+")")
	fs.UintVar(&webrtcUDPPortMax, FlagWebRTCUDPPortMax, webrtcUDPPortMax, "Max UDP port for WebRTC ICE (0 = unset; env "+EnvWebRTCUDPPortMax+")")
	fs.StringVar(&webrtcUDPListenIPStr, FlagWebRTCUDPListenIP, webrtcUDPListenIPStr, "Local listen IP for WebRTC ICE UDP sockets (env "+EnvWebRTCUDPListenIP+")")
	fs.StringVar(&webrtcNAT1To1IPsStr, FlagWebRTCNAT1To1IPs, webrtcNAT1To1IPsStr, "Comma-separated public IPs to advertise for WebRTC ICE (env "+EnvWebRTCNAT1To1IPs+")")
	fs.StringVar(&webrtcNAT1To1CandidateTypeStr, FlagWebRTCNAT1To1IPCandidateType, webrtcNAT1To1CandidateTypeStr, "Candidate type for NAT 1:1 IPs: host or srflx (env "+EnvWebRTCNAT1To1IPCandidateType+")")

	fs.IntVar(&maxSessions, "max-sessions", maxSessions, "Maximum concurrent sessions (0 = unlimited)")
	fs.IntVar(&maxUDPPpsPerSession, "max-udp-pps-per-session", maxUDPPpsPerSession, "Outbound UDP packets/sec per session (0 = unlimited)")
	fs.IntVar(&maxUDPBpsPerSession, "max-udp-bps-per-session", maxUDPBpsPerSession, "Outbound UDP bytes/sec per session (0 = unlimited)")
	fs.IntVar(&maxUDPPpsPerDest, "max-udp-pps-per-dest", maxUDPPpsPerDest, "Outbound UDP packets/sec per destination per session (0 = unlimited)")
	fs.IntVar(&maxUDPBindingsPerSession, "max-udp-bindings-per-session", maxUDPBindingsPerSession, "Maximum UDP bindings per session (0 = unlimited)")
	fs.IntVar(&maxUniqueDestinationsPerSession, "max-unique-destinations-per-session", maxUniqueDestinationsPerSession, "Maximum unique UDP destinations per session (0 = unlimited)")
	fs.IntVar(&maxDataChannelBpsPerSession, "max-dc-bps-per-session", maxDataChannelBpsPerSession, "DataChannel bytes/sec per session (relay -> client) (0 = unlimited)")
	fs.IntVar(&hardCloseAfterViolations, "hard-close-after-violations", hardCloseAfterViolations, "Close session after N rate/quota violations (0 = disabled)")
	fs.DurationVar(&violationWindow, "violation-window", violationWindow, "Violation window for hard close")

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

	var webrtcUDPPortRange *UDPPortRange
	if webrtcUDPPortMin != 0 || webrtcUDPPortMax != 0 {
		if webrtcUDPPortMin == 0 || webrtcUDPPortMax == 0 {
			return Config{}, fmt.Errorf("%s/%s and %s/%s must be set together (or both unset)",
				EnvWebRTCUDPPortMin, "--"+FlagWebRTCUDPPortMin,
				EnvWebRTCUDPPortMax, "--"+FlagWebRTCUDPPortMax,
			)
		}
		min, err := parsePortUint(webrtcUDPPortMin)
		if err != nil {
			return Config{}, fmt.Errorf("%s/%s: %w", EnvWebRTCUDPPortMin, "--"+FlagWebRTCUDPPortMin, err)
		}
		max, err := parsePortUint(webrtcUDPPortMax)
		if err != nil {
			return Config{}, fmt.Errorf("%s/%s: %w", EnvWebRTCUDPPortMax, "--"+FlagWebRTCUDPPortMax, err)
		}
		if min > max {
			return Config{}, fmt.Errorf("WebRTC UDP port range min (%d) must be <= max (%d)", min, max)
		}
		size := int(max) - int(min) + 1
		if size < RecommendedWebRTCUDPPortRangeSize {
			return Config{}, fmt.Errorf("WebRTC UDP port range is too small: %d ports (min %d recommended)", size, RecommendedWebRTCUDPPortRangeSize)
		}
		webrtcUDPPortRange = &UDPPortRange{Min: min, Max: max}
	}

	webrtcUDPListenIP := net.ParseIP(strings.TrimSpace(webrtcUDPListenIPStr))
	if webrtcUDPListenIP == nil {
		return Config{}, fmt.Errorf("invalid %s/%s %q", EnvWebRTCUDPListenIP, "--"+FlagWebRTCUDPListenIP, webrtcUDPListenIPStr)
	}

	var webrtcNAT1To1IPs []string
	if strings.TrimSpace(webrtcNAT1To1IPsStr) != "" {
		ips, err := parseIPList(webrtcNAT1To1IPsStr)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s/%s %q: %w", EnvWebRTCNAT1To1IPs, "--"+FlagWebRTCNAT1To1IPs, webrtcNAT1To1IPsStr, err)
		}
		webrtcNAT1To1IPs = ips
	}

	if strings.TrimSpace(webrtcNAT1To1CandidateTypeStr) == "" {
		webrtcNAT1To1CandidateTypeStr = string(NAT1To1CandidateTypeHost)
	}
	webrtcNAT1To1CandidateType, err := parseCandidateType(webrtcNAT1To1CandidateTypeStr)
	if err != nil {
		return Config{}, fmt.Errorf("invalid %s/%s %q: %w", EnvWebRTCNAT1To1IPCandidateType, "--"+FlagWebRTCNAT1To1IPCandidateType, webrtcNAT1To1CandidateTypeStr, err)
	}

	allowedOrigins, err := parseAllowedOrigins(allowedOriginsStr)
	if err != nil {
		return Config{}, fmt.Errorf("%s/%s: %w", EnvAllowedOrigins, "--allowed-origins", err)
	}

	cfg := Config{
		ListenAddr:      listenAddr,
		PublicBaseURL:   publicBaseURL,
		AllowedOrigins:  allowedOrigins,
		LogFormat:       logFormat,
		LogLevel:        level,
		ShutdownTimeout: shutdownTimeout,
		Mode:            mode,

		WebRTCUDPPortRange:           webrtcUDPPortRange,
		WebRTCUDPListenIP:            webrtcUDPListenIP,
		WebRTCNAT1To1IPs:             webrtcNAT1To1IPs,
		WebRTCNAT1To1IPCandidateType: webrtcNAT1To1CandidateType,

		MaxSessions:                     maxSessions,
		MaxUDPPpsPerSession:             maxUDPPpsPerSession,
		MaxUDPBpsPerSession:             maxUDPBpsPerSession,
		MaxUDPPpsPerDest:                maxUDPPpsPerDest,
		MaxUDPBindingsPerSession:        maxUDPBindingsPerSession,
		MaxUniqueDestinationsPerSession: maxUniqueDestinationsPerSession,
		MaxDataChannelBpsPerSession:     maxDataChannelBpsPerSession,
		HardCloseAfterViolations:        hardCloseAfterViolations,
		ViolationWindow:                 violationWindow,
	}

	iceServers, err := parseICEServersFromValues(iceServersJSON, stunURLs, turnURLs, turnUsername, turnCredential)
	if err != nil {
		cfg.iceConfigErr = err
	} else {
		cfg.ICEServers = iceServers
	}

	return cfg, nil
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

func envIntOrDefault(lookup func(string) (string, bool), key string, fallback int) (int, error) {
	raw, ok := lookup(key)
	if !ok || strings.TrimSpace(raw) == "" {
		return fallback, nil
	}
	n, err := strconv.Atoi(strings.TrimSpace(raw))
	if err != nil {
		return 0, fmt.Errorf("invalid %s %q: %w", key, raw, err)
	}
	return n, nil
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

func IsUnspecifiedIP(ip net.IP) bool {
	return ip == nil || ip.Equal(net.IPv4zero) || ip.Equal(net.IPv6zero)
}

func parseAllowedOrigins(raw string) ([]string, error) {
	if strings.TrimSpace(raw) == "" {
		return nil, nil
	}

	var out []string
	for _, entry := range strings.Split(raw, ",") {
		entry = strings.TrimSpace(entry)
		if entry == "" {
			continue
		}

		if entry == "*" || entry == "null" {
			out = append(out, entry)
			continue
		}

		u, err := url.Parse(entry)
		if err != nil || u.Scheme == "" || u.Host == "" {
			return nil, fmt.Errorf("invalid origin %q (expected full origin like https://example.com)", entry)
		}

		out = append(out, strings.ToLower(u.Scheme)+"://"+strings.ToLower(u.Host))
	}

	return out, nil
}

func parsePortString(s string) (uint16, error) {
	v, err := strconv.ParseUint(strings.TrimSpace(s), 10, 16)
	if err != nil {
		return 0, fmt.Errorf("invalid port %q", s)
	}
	return parsePortUint(uint(v))
}

func parsePortUint(v uint) (uint16, error) {
	if v == 0 || v > 65535 {
		return 0, fmt.Errorf("port %d out of range (1-65535)", v)
	}
	return uint16(v), nil
}

func parseCandidateType(s string) (NAT1To1IPCandidateType, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case string(NAT1To1CandidateTypeHost):
		return NAT1To1CandidateTypeHost, nil
	case string(NAT1To1CandidateTypeSrflx):
		return NAT1To1CandidateTypeSrflx, nil
	default:
		return "", fmt.Errorf("unknown candidate type %q", s)
	}
}

func parseIPList(s string) ([]string, error) {
	var out []string
	for _, raw := range strings.Split(s, ",") {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		ip := net.ParseIP(raw)
		if ip == nil {
			return nil, fmt.Errorf("invalid IP %q", raw)
		}
		out = append(out, ip.String())
	}
	if len(out) == 0 {
		return nil, fmt.Errorf("must include at least one IP")
	}
	return out, nil
}
