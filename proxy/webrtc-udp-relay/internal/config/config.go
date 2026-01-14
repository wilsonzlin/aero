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

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/origin"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

const (
	envVarListenAddr          = "AERO_WEBRTC_UDP_RELAY_LISTEN_ADDR"
	envVarPublicBaseURL       = "AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL"
	envVarAllowedOrigins      = "ALLOWED_ORIGINS"
	envVarLogFormat           = "AERO_WEBRTC_UDP_RELAY_LOG_FORMAT"
	envVarLogLevel            = "AERO_WEBRTC_UDP_RELAY_LOG_LEVEL"
	envVarShutdownTimeout     = "AERO_WEBRTC_UDP_RELAY_SHUTDOWN_TIMEOUT"
	envVarMode                = "AERO_WEBRTC_UDP_RELAY_MODE"
	envVarICEGatheringTimeout = "AERO_WEBRTC_UDP_RELAY_ICE_GATHERING_TIMEOUT"

	// Relay engine knobs.
	envVarUDPBindingIdleTimeout         = "UDP_BINDING_IDLE_TIMEOUT"
	envVarUDPInboundFilterMode          = "UDP_INBOUND_FILTER_MODE"
	envVarUDPRemoteAllowlistIdleTimeout = "UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT"
	envVarUDPReadBufferBytes            = "UDP_READ_BUFFER_BYTES"
	envVarDataChannelSendQueueBytes     = "DATACHANNEL_SEND_QUEUE_BYTES"
	envVarMaxDatagramPayloadBytes       = "MAX_DATAGRAM_PAYLOAD_BYTES"
	envVarMaxAllowedRemotesPerBinding   = "MAX_ALLOWED_REMOTES_PER_BINDING"
	envVarPreferV2                      = "PREFER_V2"

	// L2 tunnel bridging (WebRTC DataChannel "l2" <-> backend WS).
	envVarL2BackendWSURL = "L2_BACKEND_WS_URL"
	// Preferred env vars for backend auth/header hardening.
	envVarL2BackendOrigin             = "L2_BACKEND_ORIGIN"
	envVarL2BackendToken              = "L2_BACKEND_TOKEN"
	envVarL2BackendWSOrigin           = "L2_BACKEND_WS_ORIGIN"
	envVarL2BackendWSToken            = "L2_BACKEND_WS_TOKEN"
	envVarL2BackendForwardOrigin      = "L2_BACKEND_FORWARD_ORIGIN"
	envVarL2BackendAuthForwardMode    = "L2_BACKEND_AUTH_FORWARD_MODE"
	envVarL2BackendOriginOverride     = "L2_BACKEND_ORIGIN_OVERRIDE"
	envVarL2BackendForwardAeroSession = "L2_BACKEND_FORWARD_AERO_SESSION"
	envVarL2MaxMessageBytes           = "L2_MAX_MESSAGE_BYTES"

	// Quota/rate limiting knobs (required by the task).
	envVarMaxSessions = "MAX_SESSIONS"
	// envVarSessionPreallocTTL controls how long sessions allocated via POST /session
	// remain reserved before being automatically released.
	envVarSessionPreallocTTL              = "SESSION_PREALLOC_TTL"
	envVarMaxUDPPpsPerSession             = "MAX_UDP_PPS_PER_SESSION"
	envVarMaxUDPBpsPerSession             = "MAX_UDP_BPS_PER_SESSION"
	envVarMaxUDPPpsPerDest                = "MAX_UDP_PPS_PER_DEST"
	envVarMaxUDPBindingsPerSession        = "MAX_UDP_BINDINGS_PER_SESSION"
	envVarMaxUniqueDestinationsPerSession = "MAX_UNIQUE_DESTINATIONS_PER_SESSION"
	envVarMaxUDPDestBucketsPerSession     = "MAX_UDP_DEST_BUCKETS_PER_SESSION"
	envVarMaxDataChannelBpsPerSession     = "MAX_DC_BPS_PER_SESSION"
	envVarHardCloseAfterViolations        = "HARD_CLOSE_AFTER_VIOLATIONS"
	envVarViolationWindowSeconds          = "VIOLATION_WINDOW_SECONDS"

	// Signaling / WebSocket auth + hardening.
	envVarAuthMode                      = "AUTH_MODE"
	envVarAPIKey                        = "API_KEY"
	envVarJWTSecret                     = "JWT_SECRET"
	envVarSignalingAuthTimeout          = "SIGNALING_AUTH_TIMEOUT"
	envVarSignalingWSIdleTimeout        = "SIGNALING_WS_IDLE_TIMEOUT"
	envVarSignalingWSPingInterval       = "SIGNALING_WS_PING_INTERVAL"
	envVarMaxSignalingMessageBytes      = "MAX_SIGNALING_MESSAGE_BYTES"
	envVarMaxSignalingMessagesPerSecond = "MAX_SIGNALING_MESSAGES_PER_SECOND"

	// WebSocket UDP relay fallback (/udp) keepalive + idle management.
	envVarUDPWSIdleTimeout  = "UDP_WS_IDLE_TIMEOUT"
	envVarUDPWSPingInterval = "UDP_WS_PING_INTERVAL"

	// coturn TURN REST (ephemeral) credentials.
	envVarTURNRESTSharedSecret   = "TURN_REST_SHARED_SECRET"
	envVarTURNRESTTTLSeconds     = "TURN_REST_TTL_SECONDS"
	envVarTURNRESTUsernamePrefix = "TURN_REST_USERNAME_PREFIX"
	envVarTURNRESTRealm          = "TURN_REST_REALM"

	DefaultListenAddr                       = "127.0.0.1:8080"
	DefaultShutdown                         = 15 * time.Second
	DefaultICEGatherTimeout                 = 2 * time.Second
	DefaultWebRTCSessionConnectTimeout      = 30 * time.Second
	DefaultViolationWindow                  = 10 * time.Second
	DefaultMode                        Mode = ModeDev
	// DefaultSessionPreallocTTL is a safety bound for POST /session to avoid
	// permanently consuming session quota due to buggy or malicious callers.
	// Must be non-zero to avoid unbounded session leaks by default.
	DefaultSessionPreallocTTL = 60 * time.Second

	DefaultUDPBindingIdleTimeout     = 60 * time.Second
	DefaultUDPInboundFilterMode      = UDPInboundFilterModeAddressAndPort
	DefaultDataChannelSendQueueBytes = 1 << 20 // 1MiB
	DefaultMaxUDPBindingsPerSession  = 128
	DefaultMaxDatagramPayloadBytes   = udpproto.DefaultMaxPayload
	// DefaultUDPReadBufferBytes is the default UDP socket read buffer size for
	// each UDP port binding (per IP family).
	//
	// The buffer is intentionally sized to MaxDatagramPayloadBytes+1, so the
	// relay can detect and drop oversized UDP datagrams instead of forwarding a
	// silently truncated payload.
	DefaultUDPReadBufferBytes          = DefaultMaxDatagramPayloadBytes + 1
	DefaultMaxAllowedRemotesPerBinding = 1024
	DefaultL2MaxMessageBytes           = 4096
	// DefaultWebRTCDataChannelMaxMessageOverheadBytes is added on top of the
	// protocol-derived minimum when computing the effective default for
	// WebRTC_DATACHANNEL_MAX_MESSAGE_BYTES.
	DefaultWebRTCDataChannelMaxMessageOverheadBytes = 256
	// DefaultWebRTCSCTPMaxReceiveBufferBytes caps the SCTP receive buffer used by
	// pion (applies before application-level message decoding).
	DefaultWebRTCSCTPMaxReceiveBufferBytes = 1 << 20 // 1MiB

	DefaultAuthMode AuthMode = AuthModeAPIKey

	DefaultSignalingAuthTimeout          = 2 * time.Second
	DefaultSignalingWSIdleTimeout        = 60 * time.Second
	DefaultSignalingWSPingInterval       = 20 * time.Second
	DefaultMaxSignalingMessageBytes      = int64(64 * 1024)
	DefaultMaxSignalingMessagesPerSecond = 50

	DefaultUDPWSIdleTimeout  = 60 * time.Second
	DefaultUDPWSPingInterval = 20 * time.Second

	DefaultTURNRESTTTLSeconds     int64  = 3600
	DefaultTURNRESTUsernamePrefix string = "aero"
)

const defaultMaxUDPDestBucketsPerSession = 1024

const (
	envVarWebRTCUDPPortMin = "WEBRTC_UDP_PORT_MIN"
	envVarWebRTCUDPPortMax = "WEBRTC_UDP_PORT_MAX"

	// envVarWebRTCSessionConnectTimeout bounds how long a server-side PeerConnection
	// may remain in a non-connected state before being closed. This prevents
	// clients from leaking PeerConnections via HTTP offer endpoints.
	envVarWebRTCSessionConnectTimeout = "WEBRTC_SESSION_CONNECT_TIMEOUT"

	envVarWebRTCNAT1To1IPs             = "WEBRTC_NAT_1TO1_IPS"
	envVarWebRTCNAT1To1IPCandidateType = "WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE"

	envVarWebRTCUDPListenIP  = "WEBRTC_UDP_LISTEN_IP"
	DefaultWebRTCUDPListenIP = "0.0.0.0"

	// WebRTC DataChannel DoS hardening.
	//
	// These settings cap inbound SCTP/DataChannel message allocation in pion
	// before DataChannel.OnMessage handlers run.
	envVarWebRTCDataChannelMaxMessageBytes = "WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES"
	envVarWebRTCSCTPMaxReceiveBufferBytes  = "WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES"
)

const (
	flagWebRTCUDPPortMin = "webrtc-udp-port-min"
	flagWebRTCUDPPortMax = "webrtc-udp-port-max"

	flagWebRTCSessionConnectTimeout = "webrtc-session-connect-timeout"

	flagWebRTCNAT1To1IPs             = "webrtc-nat-1to1-ips"
	flagWebRTCNAT1To1IPCandidateType = "webrtc-nat-1to1-ip-candidate-type"

	flagWebRTCUDPListenIP = "webrtc-udp-listen-ip"

	flagWebRTCDataChannelMaxMessageBytes = "webrtc-datachannel-max-message-bytes"
	flagWebRTCSCTPMaxReceiveBufferBytes  = "webrtc-sctp-max-receive-buffer-bytes"
)

// recommendedWebRTCUDPPortRangeSize is an intentionally conservative minimum.
// Each WebRTC session may consume multiple UDP ports (depending on ICE
// settings), and running out of ports manifests as hard-to-debug connectivity
// failures.
const recommendedWebRTCUDPPortRangeSize = 100

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

type AuthMode string

const (
	AuthModeNone   AuthMode = "none"
	AuthModeAPIKey AuthMode = "api_key"
	AuthModeJWT    AuthMode = "jwt"
)

type L2BackendAuthForwardMode string

const (
	L2BackendAuthForwardModeNone        L2BackendAuthForwardMode = "none"
	L2BackendAuthForwardModeQuery       L2BackendAuthForwardMode = "query"
	L2BackendAuthForwardModeSubprotocol L2BackendAuthForwardMode = "subprotocol"
)

type NAT1To1IPCandidateType string

const (
	NAT1To1CandidateTypeHost  NAT1To1IPCandidateType = "host"
	NAT1To1CandidateTypeSrflx NAT1To1IPCandidateType = "srflx"
)

type UDPInboundFilterMode string

const (
	UDPInboundFilterModeAny            UDPInboundFilterMode = "any"
	UDPInboundFilterModeAddressAndPort UDPInboundFilterMode = "address_and_port"
)

type UDPPortRange struct {
	Min uint16
	Max uint16
}

type TurnRESTConfig struct {
	SharedSecret   string
	TTLSeconds     int64
	UsernamePrefix string
	Realm          string
}

func (c TurnRESTConfig) Enabled() bool {
	return strings.TrimSpace(c.SharedSecret) != ""
}

type Config struct {
	ListenAddr          string
	PublicBaseURL       string
	AllowedOrigins      []string
	LogFormat           LogFormat
	LogLevel            slog.Level
	ShutdownTimeout     time.Duration
	ICEGatheringTimeout time.Duration
	// WebRTCSessionConnectTimeout bounds how long a server-side PeerConnection may
	// remain unconnected before being closed.
	WebRTCSessionConnectTimeout time.Duration
	Mode                        Mode

	// Signaling / WebSocket auth + hardening.
	AuthMode  AuthMode
	APIKey    string
	JWTSecret string

	SignalingAuthTimeout    time.Duration
	SignalingWSIdleTimeout  time.Duration
	SignalingWSPingInterval time.Duration

	MaxSignalingMessageBytes      int64
	MaxSignalingMessagesPerSecond int

	UDPWSIdleTimeout  time.Duration
	UDPWSPingInterval time.Duration

	// Relay engine limits.
	UDPBindingIdleTimeout         time.Duration
	UDPInboundFilterMode          UDPInboundFilterMode
	UDPRemoteAllowlistIdleTimeout time.Duration
	UDPReadBufferBytes            int
	DataChannelSendQueueBytes     int
	MaxDatagramPayloadBytes       int
	MaxAllowedRemotesPerBinding   int
	PreferV2                      bool

	// L2 tunnel bridging.
	L2BackendWSURL              string
	L2BackendWSOrigin           string
	L2BackendWSToken            string
	L2BackendForwardOrigin      bool
	L2BackendAuthForwardMode    L2BackendAuthForwardMode
	L2BackendForwardAeroSession bool
	L2MaxMessageBytes           int

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

	// WebRTCDataChannelMaxMessageBytes caps the maximum size of any inbound SCTP
	// user message that the relay advertises as acceptable in SDP
	// (`a=max-message-size`).
	//
	// This is a best-effort guardrail for well-behaved peers (they should not
	// send messages larger than this), but it is not a hard receive-side cap
	// against malicious peers. The receive-side memory bound is controlled by
	// WebRTCSCTPMaxReceiveBufferBytes.
	WebRTCDataChannelMaxMessageBytes int

	// WebRTCSCTPMaxReceiveBufferBytes caps the SCTP receive buffer size used by
	// pion for a single association.
	//
	// This bounds how much data a peer can cause pion/SCTP to buffer/reassemble
	// before application-level DataChannel.OnMessage handlers run.
	WebRTCSCTPMaxReceiveBufferBytes int

	// Quotas/rate limiting.
	//
	// A value <= 0 generally means "unlimited" / disabled.
	MaxSessions int
	// SessionPreallocTTL controls how long sessions allocated via POST /session
	// remain reserved before being automatically released.
	SessionPreallocTTL              time.Duration
	MaxUDPPpsPerSession             int
	MaxUDPBpsPerSession             int
	MaxUDPPpsPerDest                int
	MaxUDPBindingsPerSession        int
	MaxUniqueDestinationsPerSession int
	MaxUDPDestBucketsPerSession     int
	MaxDataChannelBpsPerSession     int
	HardCloseAfterViolations        int
	ViolationWindow                 time.Duration
	ICEServers                      []webrtc.ICEServer
	TURNREST                        TurnRESTConfig

	iceConfigErr error
}

func (c Config) ICEConfigError() error {
	return c.iceConfigErr
}

// PeerConnectionICEServers returns the ICE server list to use when constructing
// server-side PeerConnections.
//
// When TURN REST is enabled, the client-facing ICE list may include TURN URLs
// without credentials (because credentials are injected per /webrtc/ice request).
// Pion requires TURN credentials for server-side usage, so we filter out TURN
// servers that don't have complete credentials.
func (c Config) PeerConnectionICEServers() []webrtc.ICEServer {
	if !c.TURNREST.Enabled() {
		return c.ICEServers
	}
	out := make([]webrtc.ICEServer, 0, len(c.ICEServers))
	for _, server := range c.ICEServers {
		if !iceServerHasTURNURL(server) {
			out = append(out, server)
			continue
		}
		if strings.TrimSpace(server.Username) == "" {
			continue
		}
		cred, ok := server.Credential.(string)
		if !ok || strings.TrimSpace(cred) == "" {
			continue
		}
		out = append(out, server)
	}
	return out
}

func Load(args []string) (Config, error) {
	return load(os.LookupEnv, args)
}

func load(lookup func(string) (string, bool), args []string) (Config, error) {
	envMode, _ := lookup(envVarMode)
	modeDefault := string(DefaultMode)
	if envMode != "" {
		modeDefault = envMode
	}

	envLogFormat, envLogFormatOK := lookup(envVarLogFormat)
	envLogFormatSet := envLogFormatOK && envLogFormat != ""
	logFormatDefault := envLogFormat
	if !envLogFormatSet {
		logFormatDefault = defaultLogFormatForMode(modeDefault)
	}

	envLogLevel, envLogLevelOK := lookup(envVarLogLevel)
	envLogLevelSet := envLogLevelOK && envLogLevel != ""
	logLevelDefault := envLogLevel
	if !envLogLevelSet {
		logLevelDefault = defaultLogLevelForMode(modeDefault)
	}

	listenAddr := envOrDefault(lookup, envVarListenAddr, DefaultListenAddr)
	publicBaseURL := envOrDefault(lookup, envVarPublicBaseURL, "")
	allowedOriginsStr := envOrDefault(lookup, envVarAllowedOrigins, "")
	iceServersJSON := envOrDefault(lookup, envICEServersJSON, "")
	stunURLs := envOrDefault(lookup, envStunURLs, "")
	turnURLs := envOrDefault(lookup, envTurnURLs, "")
	turnUsername := envOrDefault(lookup, envTurnUsername, "")
	turnCredential := envOrDefault(lookup, envTurnCredential, "")

	preferV2 := false
	if raw, ok := lookup(envVarPreferV2); ok && strings.TrimSpace(raw) != "" {
		v, err := strconv.ParseBool(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarPreferV2, raw, err)
		}
		preferV2 = v
	}
	turnRESTSharedSecret := envOrDefault(lookup, envVarTURNRESTSharedSecret, "")
	turnRESTTTLSeconds := DefaultTURNRESTTTLSeconds
	if raw, ok := lookup(envVarTURNRESTTTLSeconds); ok && strings.TrimSpace(raw) != "" {
		n, err := strconv.ParseInt(strings.TrimSpace(raw), 10, 64)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarTURNRESTTTLSeconds, raw, err)
		}
		turnRESTTTLSeconds = n
	}
	turnRESTUsernamePrefix := envOrDefault(lookup, envVarTURNRESTUsernamePrefix, DefaultTURNRESTUsernamePrefix)
	turnRESTRealm := envOrDefault(lookup, envVarTURNRESTRealm, "")

	udpBindingIdleTimeout := DefaultUDPBindingIdleTimeout
	if raw, ok := lookup(envVarUDPBindingIdleTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarUDPBindingIdleTimeout, raw, err)
		}
		udpBindingIdleTimeout = d
	}

	udpInboundFilterModeStr := envOrDefault(lookup, envVarUDPInboundFilterMode, string(DefaultUDPInboundFilterMode))
	udpRemoteAllowlistIdleTimeout := udpBindingIdleTimeout
	envAllowlistTTL, envAllowlistTTLOK := lookup(envVarUDPRemoteAllowlistIdleTimeout)
	envAllowlistTTLSet := envAllowlistTTLOK && strings.TrimSpace(envAllowlistTTL) != ""
	if envAllowlistTTLSet {
		d, err := time.ParseDuration(strings.TrimSpace(envAllowlistTTL))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarUDPRemoteAllowlistIdleTimeout, envAllowlistTTL, err)
		}
		udpRemoteAllowlistIdleTimeout = d
	}

	maxDatagramPayloadBytes, err := envIntOrDefault(lookup, envVarMaxDatagramPayloadBytes, DefaultMaxDatagramPayloadBytes)
	if err != nil {
		return Config{}, err
	}
	// Track whether the UDP read buffer size was explicitly configured so we can
	// derive a default from MAX_DATAGRAM_PAYLOAD_BYTES when unset.
	envUDPReadBufferBytes, envUDPReadBufferBytesOK := lookup(envVarUDPReadBufferBytes)
	envUDPReadBufferBytesSet := envUDPReadBufferBytesOK && strings.TrimSpace(envUDPReadBufferBytes) != ""

	udpReadBufferBytes, err := envIntOrDefault(lookup, envVarUDPReadBufferBytes, maxDatagramPayloadBytes+1)
	if err != nil {
		return Config{}, err
	}
	dataChannelSendQueueBytes, err := envIntOrDefault(lookup, envVarDataChannelSendQueueBytes, DefaultDataChannelSendQueueBytes)
	if err != nil {
		return Config{}, err
	}
	maxAllowedRemotesPerBinding, err := envIntOrDefault(lookup, envVarMaxAllowedRemotesPerBinding, DefaultMaxAllowedRemotesPerBinding)
	if err != nil {
		return Config{}, err
	}
	l2BackendWSURL := envOrDefault(lookup, envVarL2BackendWSURL, "")
	l2BackendWSOrigin := envOrDefault(lookup, envVarL2BackendWSOrigin, "")
	l2BackendWSToken := envOrDefault(lookup, envVarL2BackendToken, envOrDefault(lookup, envVarL2BackendWSToken, ""))
	l2BackendOriginOverride := envOrDefault(lookup, envVarL2BackendOrigin, envOrDefault(lookup, envVarL2BackendOriginOverride, ""))

	l2BackendForwardAeroSession := false
	if raw, ok := lookup(envVarL2BackendForwardAeroSession); ok && strings.TrimSpace(raw) != "" {
		v, err := strconv.ParseBool(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarL2BackendForwardAeroSession, raw, err)
		}
		l2BackendForwardAeroSession = v
	}

	l2BackendForwardOrigin := false
	envForwardOrigin, envForwardOriginOK := lookup(envVarL2BackendForwardOrigin)
	envForwardOriginSet := envForwardOriginOK && strings.TrimSpace(envForwardOrigin) != ""
	if envForwardOriginSet {
		v, err := strconv.ParseBool(strings.TrimSpace(envForwardOrigin))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarL2BackendForwardOrigin, envForwardOrigin, err)
		}
		l2BackendForwardOrigin = v
	}

	l2BackendAuthForwardModeStr := string(L2BackendAuthForwardModeQuery)
	envAuthForwardMode, envAuthForwardModeOK := lookup(envVarL2BackendAuthForwardMode)
	envAuthForwardModeSet := envAuthForwardModeOK && strings.TrimSpace(envAuthForwardMode) != ""
	if envAuthForwardModeSet {
		l2BackendAuthForwardModeStr = strings.TrimSpace(envAuthForwardMode)
	}
	l2MaxMessageBytes, err := envIntOrDefault(lookup, envVarL2MaxMessageBytes, DefaultL2MaxMessageBytes)
	if err != nil {
		return Config{}, err
	}
	webrtcDataChannelMaxMessageBytes, err := envIntOrDefault(lookup, envVarWebRTCDataChannelMaxMessageBytes, 0)
	if err != nil {
		return Config{}, err
	}
	webrtcSCTPMaxReceiveBufferBytes, err := envIntOrDefault(lookup, envVarWebRTCSCTPMaxReceiveBufferBytes, 0)
	if err != nil {
		return Config{}, err
	}

	shutdownTimeout := DefaultShutdown
	if raw, ok := lookup(envVarShutdownTimeout); ok && raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarShutdownTimeout, raw, err)
		}
		shutdownTimeout = d
	}

	iceGatherTimeout := DefaultICEGatherTimeout
	if raw, ok := lookup(envVarICEGatheringTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarICEGatheringTimeout, raw, err)
		}
		iceGatherTimeout = d
	}

	webrtcSessionConnectTimeout := DefaultWebRTCSessionConnectTimeout
	if raw, ok := lookup(envVarWebRTCSessionConnectTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarWebRTCSessionConnectTimeout, raw, err)
		}
		webrtcSessionConnectTimeout = d
	}

	maxSessions, err := envIntOrDefault(lookup, envVarMaxSessions, 0)
	if err != nil {
		return Config{}, err
	}
	sessionPreallocTTL := DefaultSessionPreallocTTL
	if raw, ok := lookup(envVarSessionPreallocTTL); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarSessionPreallocTTL, raw, err)
		}
		sessionPreallocTTL = d
	}
	maxUDPPpsPerSession, err := envIntOrDefault(lookup, envVarMaxUDPPpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPBpsPerSession, err := envIntOrDefault(lookup, envVarMaxUDPBpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPPpsPerDest, err := envIntOrDefault(lookup, envVarMaxUDPPpsPerDest, 0)
	if err != nil {
		return Config{}, err
	}
	maxUDPBindingsPerSession, err := envIntOrDefault(lookup, envVarMaxUDPBindingsPerSession, DefaultMaxUDPBindingsPerSession)
	if err != nil {
		return Config{}, err
	}
	maxUniqueDestinationsPerSession, err := envIntOrDefault(lookup, envVarMaxUniqueDestinationsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	envMaxUDPDestBuckets, envMaxUDPDestBucketsOK := lookup(envVarMaxUDPDestBucketsPerSession)
	envMaxUDPDestBucketsSet := envMaxUDPDestBucketsOK && strings.TrimSpace(envMaxUDPDestBuckets) != ""
	maxUDPDestBucketsPerSession, err := envIntOrDefault(lookup, envVarMaxUDPDestBucketsPerSession, defaultMaxUDPDestBucketsPerSession)
	if err != nil {
		return Config{}, err
	}
	maxDataChannelBpsPerSession, err := envIntOrDefault(lookup, envVarMaxDataChannelBpsPerSession, 0)
	if err != nil {
		return Config{}, err
	}
	hardCloseAfterViolations, err := envIntOrDefault(lookup, envVarHardCloseAfterViolations, 0)
	if err != nil {
		return Config{}, err
	}

	violationWindow := DefaultViolationWindow
	if raw, ok := lookup(envVarViolationWindowSeconds); ok && strings.TrimSpace(raw) != "" {
		seconds, err := strconv.Atoi(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarViolationWindowSeconds, raw, err)
		}
		if seconds > 0 {
			violationWindow = time.Duration(seconds) * time.Second
		}
	}

	authModeDefault := string(DefaultAuthMode)
	if raw, ok := lookup(envVarAuthMode); ok && strings.TrimSpace(raw) != "" {
		authModeDefault = strings.TrimSpace(raw)
	}

	apiKey := envOrDefault(lookup, envVarAPIKey, "")
	jwtSecret := envOrDefault(lookup, envVarJWTSecret, "")

	signalingAuthTimeout := DefaultSignalingAuthTimeout
	if raw, ok := lookup(envVarSignalingAuthTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarSignalingAuthTimeout, raw, err)
		}
		signalingAuthTimeout = d
	}

	signalingWSIdleTimeout := DefaultSignalingWSIdleTimeout
	if raw, ok := lookup(envVarSignalingWSIdleTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarSignalingWSIdleTimeout, raw, err)
		}
		signalingWSIdleTimeout = d
	}

	signalingWSPingInterval := DefaultSignalingWSPingInterval
	if raw, ok := lookup(envVarSignalingWSPingInterval); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarSignalingWSPingInterval, raw, err)
		}
		signalingWSPingInterval = d
	}

	maxSignalingMessageBytes := DefaultMaxSignalingMessageBytes
	if raw, ok := lookup(envVarMaxSignalingMessageBytes); ok && strings.TrimSpace(raw) != "" {
		n, err := strconv.ParseInt(strings.TrimSpace(raw), 10, 64)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarMaxSignalingMessageBytes, raw, err)
		}
		maxSignalingMessageBytes = n
	}

	maxSignalingMessagesPerSecond := DefaultMaxSignalingMessagesPerSecond
	if raw, ok := lookup(envVarMaxSignalingMessagesPerSecond); ok && strings.TrimSpace(raw) != "" {
		n, err := strconv.Atoi(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarMaxSignalingMessagesPerSecond, raw, err)
		}
		maxSignalingMessagesPerSecond = n
	}

	udpWSIdleTimeout := DefaultUDPWSIdleTimeout
	if raw, ok := lookup(envVarUDPWSIdleTimeout); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarUDPWSIdleTimeout, raw, err)
		}
		udpWSIdleTimeout = d
	}

	udpWSPingInterval := DefaultUDPWSPingInterval
	if raw, ok := lookup(envVarUDPWSPingInterval); ok && strings.TrimSpace(raw) != "" {
		d, err := time.ParseDuration(strings.TrimSpace(raw))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarUDPWSPingInterval, raw, err)
		}
		udpWSPingInterval = d
	}

	// WebRTC network defaults (env values become flag defaults).
	var webrtcUDPPortMin uint
	if raw, ok := lookup(envVarWebRTCUDPPortMin); ok && strings.TrimSpace(raw) != "" {
		p, err := parsePortString(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarWebRTCUDPPortMin, raw, err)
		}
		webrtcUDPPortMin = uint(p)
	}

	var webrtcUDPPortMax uint
	if raw, ok := lookup(envVarWebRTCUDPPortMax); ok && strings.TrimSpace(raw) != "" {
		p, err := parsePortString(raw)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s %q: %w", envVarWebRTCUDPPortMax, raw, err)
		}
		webrtcUDPPortMax = uint(p)
	}

	if (webrtcUDPPortMin == 0) != (webrtcUDPPortMax == 0) {
		return Config{}, fmt.Errorf("%s and %s must be set together (or both unset)", envVarWebRTCUDPPortMin, envVarWebRTCUDPPortMax)
	}

	webrtcUDPListenIPStr := envOrDefault(lookup, envVarWebRTCUDPListenIP, DefaultWebRTCUDPListenIP)
	webrtcNAT1To1IPsStr := envOrDefault(lookup, envVarWebRTCNAT1To1IPs, "")
	webrtcNAT1To1CandidateTypeStr := envOrDefault(lookup, envVarWebRTCNAT1To1IPCandidateType, string(NAT1To1CandidateTypeHost))
	fs := flag.NewFlagSet("aero-webrtc-udp-relay", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)

	var (
		modeStr                      string
		logFormatStr                 string
		logLevelStr                  string
		authModeStr                  string
		l2BackendAuthForwardModeFlag string
	)

	fs.StringVar(&listenAddr, "listen-addr", listenAddr, "HTTP listen address (host:port)")
	fs.StringVar(&publicBaseURL, "public-base-url", publicBaseURL, "Public base URL (optional; used for logging)")
	fs.StringVar(&allowedOriginsStr, "allowed-origins", allowedOriginsStr, "Comma-separated list of allowed browser origins (env "+envVarAllowedOrigins+")")
	fs.StringVar(&modeStr, "mode", modeDefault, "Run mode: dev or prod")
	fs.StringVar(&logFormatStr, "log-format", logFormatDefault, "Log format: text or json")
	fs.StringVar(&logLevelStr, "log-level", logLevelDefault, "Log level: debug, info, warn, error")
	fs.DurationVar(&shutdownTimeout, "shutdown-timeout", shutdownTimeout, "Graceful shutdown timeout (e.g. 15s)")
	fs.DurationVar(&iceGatherTimeout, "ice-gather-timeout", iceGatherTimeout, "Max time to wait for ICE gathering on non-trickle HTTP signaling endpoints (/offer, /webrtc/offer) (e.g. 2s)")
	fs.DurationVar(&webrtcSessionConnectTimeout, flagWebRTCSessionConnectTimeout, webrtcSessionConnectTimeout, "Max time to wait for WebRTC sessions to connect before closing (env "+envVarWebRTCSessionConnectTimeout+")")
	fs.StringVar(&iceServersJSON, "ice-servers-json", iceServersJSON, "ICE server JSON config (AERO_ICE_SERVERS_JSON)")
	fs.StringVar(&stunURLs, "stun-urls", stunURLs, "comma-separated STUN URLs (AERO_STUN_URLS)")
	fs.StringVar(&turnURLs, "turn-urls", turnURLs, "comma-separated TURN URLs (AERO_TURN_URLS)")
	fs.StringVar(&turnUsername, "turn-username", turnUsername, "TURN username (AERO_TURN_USERNAME)")
	fs.StringVar(&turnCredential, "turn-credential", turnCredential, "TURN credential (AERO_TURN_CREDENTIAL)")
	fs.StringVar(&turnRESTSharedSecret, "turn-rest-shared-secret", turnRESTSharedSecret, "TURN REST shared secret ("+envVarTURNRESTSharedSecret+")")
	fs.Int64Var(&turnRESTTTLSeconds, "turn-rest-ttl-seconds", turnRESTTTLSeconds, "TURN REST credential TTL seconds ("+envVarTURNRESTTTLSeconds+")")
	fs.StringVar(&turnRESTUsernamePrefix, "turn-rest-username-prefix", turnRESTUsernamePrefix, "TURN REST username prefix ("+envVarTURNRESTUsernamePrefix+")")
	fs.StringVar(&turnRESTRealm, "turn-rest-realm", turnRESTRealm, "TURN realm (coturn config; "+envVarTURNRESTRealm+")")

	fs.UintVar(&webrtcUDPPortMin, flagWebRTCUDPPortMin, webrtcUDPPortMin, "Min UDP port for WebRTC ICE (0 = unset; env "+envVarWebRTCUDPPortMin+")")
	fs.UintVar(&webrtcUDPPortMax, flagWebRTCUDPPortMax, webrtcUDPPortMax, "Max UDP port for WebRTC ICE (0 = unset; env "+envVarWebRTCUDPPortMax+")")
	fs.StringVar(&webrtcUDPListenIPStr, flagWebRTCUDPListenIP, webrtcUDPListenIPStr, "Local listen IP for WebRTC ICE UDP sockets (env "+envVarWebRTCUDPListenIP+")")
	fs.StringVar(&webrtcNAT1To1IPsStr, flagWebRTCNAT1To1IPs, webrtcNAT1To1IPsStr, "Comma-separated public IPs to advertise for WebRTC ICE (env "+envVarWebRTCNAT1To1IPs+")")
	fs.StringVar(&webrtcNAT1To1CandidateTypeStr, flagWebRTCNAT1To1IPCandidateType, webrtcNAT1To1CandidateTypeStr, "Candidate type for NAT 1:1 IPs: host or srflx (env "+envVarWebRTCNAT1To1IPCandidateType+")")
	fs.IntVar(&webrtcDataChannelMaxMessageBytes, flagWebRTCDataChannelMaxMessageBytes, webrtcDataChannelMaxMessageBytes, "Max inbound WebRTC DataChannel message size in bytes (0 = auto; env "+envVarWebRTCDataChannelMaxMessageBytes+")")
	fs.IntVar(&webrtcSCTPMaxReceiveBufferBytes, flagWebRTCSCTPMaxReceiveBufferBytes, webrtcSCTPMaxReceiveBufferBytes, "Max SCTP receive buffer size in bytes (0 = auto; env "+envVarWebRTCSCTPMaxReceiveBufferBytes+")")

	fs.IntVar(&maxSessions, "max-sessions", maxSessions, "Maximum concurrent sessions (0 = unlimited)")
	fs.DurationVar(&sessionPreallocTTL, "session-prealloc-ttl", sessionPreallocTTL, "TTL for sessions allocated via POST /session (env "+envVarSessionPreallocTTL+")")
	fs.IntVar(&maxUDPPpsPerSession, "max-udp-pps-per-session", maxUDPPpsPerSession, "Outbound UDP packets/sec per session (0 = unlimited)")
	fs.IntVar(&maxUDPBpsPerSession, "max-udp-bps-per-session", maxUDPBpsPerSession, "Outbound UDP bytes/sec per session (0 = unlimited)")
	fs.IntVar(&maxUDPPpsPerDest, "max-udp-pps-per-dest", maxUDPPpsPerDest, "Outbound UDP packets/sec per destination per session (0 = unlimited)")
	fs.IntVar(&maxUDPBindingsPerSession, "max-udp-bindings-per-session", maxUDPBindingsPerSession, "Maximum UDP bindings per session (env "+envVarMaxUDPBindingsPerSession+")")
	fs.IntVar(&maxUniqueDestinationsPerSession, "max-unique-destinations-per-session", maxUniqueDestinationsPerSession, "Maximum unique UDP destinations per session (0 = unlimited)")
	fs.IntVar(&maxUDPDestBucketsPerSession, "max-udp-dest-buckets-per-session", maxUDPDestBucketsPerSession, "Maximum per-destination UDP rate limiter buckets per session (env "+envVarMaxUDPDestBucketsPerSession+")")
	fs.IntVar(&maxDataChannelBpsPerSession, "max-dc-bps-per-session", maxDataChannelBpsPerSession, "DataChannel bytes/sec per session (relay -> client) (0 = unlimited)")
	fs.IntVar(&hardCloseAfterViolations, "hard-close-after-violations", hardCloseAfterViolations, "Close session after N rate/quota violations (0 = disabled)")
	fs.DurationVar(&violationWindow, "violation-window", violationWindow, "Violation window for hard close")

	fs.BoolVar(&preferV2, "prefer-v2", preferV2, "Prefer v2 relay->client frames once the client demonstrates v2 support (env "+envVarPreferV2+")")
	fs.DurationVar(&udpBindingIdleTimeout, "udp-binding-idle-timeout", udpBindingIdleTimeout, "Close idle UDP bindings after this duration (env "+envVarUDPBindingIdleTimeout+")")
	fs.StringVar(&udpInboundFilterModeStr, "udp-inbound-filter-mode", udpInboundFilterModeStr, "Inbound UDP filtering: any (full-cone) or address_and_port (recommended) (env "+envVarUDPInboundFilterMode+")")
	fs.DurationVar(&udpRemoteAllowlistIdleTimeout, "udp-remote-allowlist-idle-timeout", udpRemoteAllowlistIdleTimeout, "Expire UDP remote allowlist entries after this duration (default: udp-binding-idle-timeout; env "+envVarUDPRemoteAllowlistIdleTimeout+")")
	fs.IntVar(&udpReadBufferBytes, "udp-read-buffer-bytes", udpReadBufferBytes, "UDP socket read buffer size in bytes (env "+envVarUDPReadBufferBytes+")")
	fs.IntVar(&dataChannelSendQueueBytes, "datachannel-send-queue-bytes", dataChannelSendQueueBytes, "Max queued outbound DataChannel bytes before dropping (env "+envVarDataChannelSendQueueBytes+")")
	fs.IntVar(&maxDatagramPayloadBytes, "max-datagram-payload-bytes", maxDatagramPayloadBytes, "Max UDP datagram payload bytes for relay frames (env "+envVarMaxDatagramPayloadBytes+")")
	fs.IntVar(&maxAllowedRemotesPerBinding, "max-allowed-remotes-per-binding", maxAllowedRemotesPerBinding, "Maximum remote endpoints tracked per UDP binding allowlist (env "+envVarMaxAllowedRemotesPerBinding+")")
	fs.StringVar(&l2BackendWSURL, "l2-backend-ws-url", l2BackendWSURL, "Backend WebSocket URL for L2 tunnel bridging (env "+envVarL2BackendWSURL+")")
	fs.StringVar(&l2BackendWSOrigin, "l2-backend-ws-origin", l2BackendWSOrigin, "Origin header value to send when dialing the L2 backend WebSocket (env "+envVarL2BackendWSOrigin+")")
	fs.StringVar(&l2BackendWSToken, "l2-backend-token", l2BackendWSToken, "Optional token to present to the L2 backend via WebSocket subprotocol (sent as "+l2tunnel.TokenSubprotocolPrefix+"<token>; env "+envVarL2BackendToken+")")
	fs.StringVar(
		&l2BackendWSToken,
		"l2-backend-ws-token",
		l2BackendWSToken,
		"Optional token to present to the L2 backend via WebSocket subprotocol (sent as "+l2tunnel.TokenSubprotocolPrefix+"<token>; env "+envVarL2BackendWSToken+")",
	)
	fs.BoolVar(&l2BackendForwardOrigin, "l2-backend-forward-origin", l2BackendForwardOrigin, "Forward Origin header when dialing the L2 backend WebSocket (env "+envVarL2BackendForwardOrigin+")")
	fs.StringVar(&l2BackendAuthForwardModeFlag, "l2-backend-auth-forward-mode", l2BackendAuthForwardModeStr, "L2 backend auth forwarding mode: none, query, subprotocol (env "+envVarL2BackendAuthForwardMode+")")
	fs.StringVar(&l2BackendOriginOverride, "l2-backend-origin", l2BackendOriginOverride, "Alias for --l2-backend-origin-override (env "+envVarL2BackendOrigin+")")
	fs.StringVar(&l2BackendOriginOverride, "l2-backend-origin-override", l2BackendOriginOverride, "Override Origin header sent to the L2 backend WebSocket (env "+envVarL2BackendOriginOverride+")")
	fs.BoolVar(&l2BackendForwardAeroSession, "l2-backend-forward-aero-session", l2BackendForwardAeroSession, "Forward the caller's aero_session cookie to the L2 backend WebSocket as Cookie: aero_session=... (env "+envVarL2BackendForwardAeroSession+")")
	fs.IntVar(&l2MaxMessageBytes, "l2-max-message-bytes", l2MaxMessageBytes, "Max L2 tunnel message size in bytes (env "+envVarL2MaxMessageBytes+")")

	fs.StringVar(&authModeStr, "auth-mode", authModeDefault, "Signaling auth mode: none, api_key, or jwt (env "+envVarAuthMode+")")
	fs.DurationVar(&signalingAuthTimeout, "signaling-auth-timeout", signalingAuthTimeout, "Signaling WS auth timeout (env "+envVarSignalingAuthTimeout+")")
	fs.DurationVar(&signalingWSIdleTimeout, "signaling-ws-idle-timeout", signalingWSIdleTimeout, "Close idle signaling WebSocket connections after this duration (env "+envVarSignalingWSIdleTimeout+")")
	fs.DurationVar(&signalingWSPingInterval, "signaling-ws-ping-interval", signalingWSPingInterval, "Send ping frames on signaling WebSocket connections at this interval (must be < --signaling-ws-idle-timeout; env "+envVarSignalingWSPingInterval+")")
	fs.Int64Var(&maxSignalingMessageBytes, "max-signaling-message-bytes", maxSignalingMessageBytes, "Max inbound signaling WS message size in bytes (env "+envVarMaxSignalingMessageBytes+")")
	fs.IntVar(&maxSignalingMessagesPerSecond, "max-signaling-messages-per-second", maxSignalingMessagesPerSecond, "Max inbound signaling WS messages per second (env "+envVarMaxSignalingMessagesPerSecond+")")
	fs.DurationVar(&udpWSIdleTimeout, "udp-ws-idle-timeout", udpWSIdleTimeout, "Close idle /udp WebSocket connections after this duration (env "+envVarUDPWSIdleTimeout+")")
	fs.DurationVar(&udpWSPingInterval, "udp-ws-ping-interval", udpWSPingInterval, "Send ping frames on /udp WebSocket connections at this interval (must be < --udp-ws-idle-timeout; env "+envVarUDPWSPingInterval+")")

	if err := fs.Parse(args); err != nil {
		return Config{}, err
	}

	setFlags := map[string]bool{}
	fs.Visit(func(f *flag.Flag) {
		setFlags[f.Name] = true
	})

	// If UDP_READ_BUFFER_BYTES/--udp-read-buffer-bytes is unset, derive it from
	// the (possibly overridden) max payload after flag parsing.
	if !envUDPReadBufferBytesSet && !setFlags["udp-read-buffer-bytes"] {
		udpReadBufferBytes = maxDatagramPayloadBytes + 1
	}
	if !envAllowlistTTLSet && !setFlags["udp-remote-allowlist-idle-timeout"] {
		udpRemoteAllowlistIdleTimeout = udpBindingIdleTimeout
	}

	if !envMaxUDPDestBucketsSet && !setFlags["max-udp-dest-buckets-per-session"] && maxUniqueDestinationsPerSession > 0 {
		// If the user configured a unique destination cap but didn't explicitly
		// configure the per-destination bucket cap, default buckets to the same
		// value to avoid surprise memory usage.
		maxUDPDestBucketsPerSession = maxUniqueDestinationsPerSession
	}

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

	authMode, err := parseAuthMode(authModeStr)
	if err != nil {
		return Config{}, err
	}

	l2BackendAuthForwardMode, err := parseL2BackendAuthForwardMode(l2BackendAuthForwardModeFlag)
	if err != nil {
		return Config{}, err
	}

	udpInboundFilterMode, err := parseUDPInboundFilterMode(udpInboundFilterModeStr)
	if err != nil {
		return Config{}, fmt.Errorf("invalid %s/--udp-inbound-filter-mode %q: %w", envVarUDPInboundFilterMode, udpInboundFilterModeStr, err)
	}

	if listenAddr == "" {
		return Config{}, fmt.Errorf("listen address must not be empty")
	}
	if shutdownTimeout <= 0 {
		return Config{}, fmt.Errorf("shutdown timeout must be > 0")
	}
	if iceGatherTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--ice-gather-timeout must be > 0", envVarICEGatheringTimeout)
	}
	if webrtcSessionConnectTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--%s must be > 0", envVarWebRTCSessionConnectTimeout, flagWebRTCSessionConnectTimeout)
	}
	if udpBindingIdleTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--udp-binding-idle-timeout must be > 0", envVarUDPBindingIdleTimeout)
	}
	if udpRemoteAllowlistIdleTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--udp-remote-allowlist-idle-timeout must be > 0", envVarUDPRemoteAllowlistIdleTimeout)
	}
	if udpReadBufferBytes <= 0 {
		return Config{}, fmt.Errorf("%s/--udp-read-buffer-bytes must be > 0", envVarUDPReadBufferBytes)
	}
	if dataChannelSendQueueBytes <= 0 {
		return Config{}, fmt.Errorf("%s/--datachannel-send-queue-bytes must be > 0", envVarDataChannelSendQueueBytes)
	}
	if maxDatagramPayloadBytes <= 0 {
		return Config{}, fmt.Errorf("%s/--max-datagram-payload-bytes must be > 0", envVarMaxDatagramPayloadBytes)
	}
	minReadBuf := maxDatagramPayloadBytes + 1
	if udpReadBufferBytes < minReadBuf {
		return Config{}, fmt.Errorf("%s/--udp-read-buffer-bytes must be >= %s+1 (%d); got %d",
			envVarUDPReadBufferBytes,
			envVarMaxDatagramPayloadBytes,
			minReadBuf,
			udpReadBufferBytes,
		)
	}
	if maxAllowedRemotesPerBinding <= 0 {
		return Config{}, fmt.Errorf("%s/--max-allowed-remotes-per-binding must be > 0", envVarMaxAllowedRemotesPerBinding)
	}
	if l2MaxMessageBytes <= 0 {
		return Config{}, fmt.Errorf("%s/--l2-max-message-bytes must be > 0", envVarL2MaxMessageBytes)
	}
	if webrtcDataChannelMaxMessageBytes < 0 {
		return Config{}, fmt.Errorf("%s/--%s must be >= 0 (0 = auto)", envVarWebRTCDataChannelMaxMessageBytes, flagWebRTCDataChannelMaxMessageBytes)
	}
	if webrtcSCTPMaxReceiveBufferBytes < 0 {
		return Config{}, fmt.Errorf("%s/--%s must be >= 0 (0 = auto)", envVarWebRTCSCTPMaxReceiveBufferBytes, flagWebRTCSCTPMaxReceiveBufferBytes)
	}

	// Derive WebRTC DataChannel/SCTP size limits if not explicitly configured.
	//
	// - WebRTCDataChannelMaxMessageBytes is advertised via SDP `a=max-message-size`
	//   (best-effort guardrail for compliant peers).
	// - WebRTCSCTPMaxReceiveBufferBytes is the receive-side hard cap that bounds
	//   how much data pion/SCTP will buffer before DataChannel.OnMessage runs.
	minDCMax := minWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
	effectiveDCMax := webrtcDataChannelMaxMessageBytes
	if effectiveDCMax == 0 {
		effectiveDCMax = defaultWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
	}
	if effectiveDCMax <= 0 {
		return Config{}, fmt.Errorf("%s/--%s must be > 0", envVarWebRTCDataChannelMaxMessageBytes, flagWebRTCDataChannelMaxMessageBytes)
	}
	if effectiveDCMax < minDCMax {
		return Config{}, fmt.Errorf("%s/--%s must be >= %d (max of MAX_DATAGRAM_PAYLOAD_BYTES+%d and L2_MAX_MESSAGE_BYTES)",
			envVarWebRTCDataChannelMaxMessageBytes,
			flagWebRTCDataChannelMaxMessageBytes,
			minDCMax,
			webrtcDataChannelUDPFrameOverheadBytes,
		)
	}

	effectiveSCTPRecvBuf := webrtcSCTPMaxReceiveBufferBytes
	if effectiveSCTPRecvBuf == 0 {
		effectiveSCTPRecvBuf = defaultWebRTCSCTPMaxReceiveBufferBytes(effectiveDCMax)
	}
	if effectiveSCTPRecvBuf <= 0 {
		return Config{}, fmt.Errorf("%s/--%s must be > 0", envVarWebRTCSCTPMaxReceiveBufferBytes, flagWebRTCSCTPMaxReceiveBufferBytes)
	}
	if effectiveSCTPRecvBuf < effectiveDCMax {
		return Config{}, fmt.Errorf("%s/--%s must be >= %d (must be >= %s)",
			envVarWebRTCSCTPMaxReceiveBufferBytes,
			flagWebRTCSCTPMaxReceiveBufferBytes,
			effectiveDCMax,
			envVarWebRTCDataChannelMaxMessageBytes,
		)
	}
	if effectiveSCTPRecvBuf < minWebRTCSCTPReceiveBufferBytes {
		return Config{}, fmt.Errorf("%s/--%s must be >= %d (SCTP receive buffer too small)",
			envVarWebRTCSCTPMaxReceiveBufferBytes,
			flagWebRTCSCTPMaxReceiveBufferBytes,
			minWebRTCSCTPReceiveBufferBytes,
		)
	}
	if maxUDPBindingsPerSession <= 0 {
		return Config{}, fmt.Errorf("%s/--max-udp-bindings-per-session must be > 0", envVarMaxUDPBindingsPerSession)
	}
	if maxUDPDestBucketsPerSession <= 0 {
		return Config{}, fmt.Errorf("%s/--max-udp-dest-buckets-per-session must be > 0", envVarMaxUDPDestBucketsPerSession)
	}
	if sessionPreallocTTL <= 0 {
		return Config{}, fmt.Errorf("%s/--session-prealloc-ttl must be > 0", envVarSessionPreallocTTL)
	}
	if authMode == AuthModeAPIKey && strings.TrimSpace(apiKey) == "" {
		return Config{}, fmt.Errorf("%s must be set when %s=%s", envVarAPIKey, envVarAuthMode, AuthModeAPIKey)
	}
	if authMode == AuthModeJWT && strings.TrimSpace(jwtSecret) == "" {
		return Config{}, fmt.Errorf("%s must be set when %s=%s", envVarJWTSecret, envVarAuthMode, AuthModeJWT)
	}
	if signalingAuthTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--signaling-auth-timeout must be > 0", envVarSignalingAuthTimeout)
	}
	if signalingWSIdleTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--signaling-ws-idle-timeout must be > 0", envVarSignalingWSIdleTimeout)
	}
	if signalingWSPingInterval <= 0 {
		return Config{}, fmt.Errorf("%s/--signaling-ws-ping-interval must be > 0", envVarSignalingWSPingInterval)
	}
	if signalingWSPingInterval >= signalingWSIdleTimeout {
		return Config{}, fmt.Errorf("%s/--signaling-ws-ping-interval must be < %s/--signaling-ws-idle-timeout", envVarSignalingWSPingInterval, envVarSignalingWSIdleTimeout)
	}
	if maxSignalingMessageBytes <= 0 {
		return Config{}, fmt.Errorf("%s/--max-signaling-message-bytes must be > 0", envVarMaxSignalingMessageBytes)
	}
	if maxSignalingMessagesPerSecond <= 0 {
		return Config{}, fmt.Errorf("%s/--max-signaling-messages-per-second must be > 0", envVarMaxSignalingMessagesPerSecond)
	}
	if udpWSIdleTimeout <= 0 {
		return Config{}, fmt.Errorf("%s/--udp-ws-idle-timeout must be > 0", envVarUDPWSIdleTimeout)
	}
	if udpWSPingInterval <= 0 {
		return Config{}, fmt.Errorf("%s/--udp-ws-ping-interval must be > 0", envVarUDPWSPingInterval)
	}
	if udpWSPingInterval >= udpWSIdleTimeout {
		return Config{}, fmt.Errorf("%s/--udp-ws-ping-interval must be < %s/--udp-ws-idle-timeout", envVarUDPWSPingInterval, envVarUDPWSIdleTimeout)
	}

	if strings.TrimSpace(turnRESTSharedSecret) != "" {
		if turnRESTTTLSeconds <= 0 {
			return Config{}, fmt.Errorf("%s must be > 0 when %s is set", envVarTURNRESTTTLSeconds, envVarTURNRESTSharedSecret)
		}
		if strings.TrimSpace(turnRESTUsernamePrefix) == "" {
			return Config{}, fmt.Errorf("%s must be non-empty when %s is set", envVarTURNRESTUsernamePrefix, envVarTURNRESTSharedSecret)
		}
		if strings.Contains(turnRESTUsernamePrefix, ":") {
			return Config{}, fmt.Errorf("%s must not contain ':'", envVarTURNRESTUsernamePrefix)
		}
	}

	var webrtcUDPPortRange *UDPPortRange
	if webrtcUDPPortMin != 0 || webrtcUDPPortMax != 0 {
		if webrtcUDPPortMin == 0 || webrtcUDPPortMax == 0 {
			return Config{}, fmt.Errorf("%s/%s and %s/%s must be set together (or both unset)",
				envVarWebRTCUDPPortMin, "--"+flagWebRTCUDPPortMin,
				envVarWebRTCUDPPortMax, "--"+flagWebRTCUDPPortMax,
			)
		}
		min, err := parsePortUint(webrtcUDPPortMin)
		if err != nil {
			return Config{}, fmt.Errorf("%s/%s: %w", envVarWebRTCUDPPortMin, "--"+flagWebRTCUDPPortMin, err)
		}
		max, err := parsePortUint(webrtcUDPPortMax)
		if err != nil {
			return Config{}, fmt.Errorf("%s/%s: %w", envVarWebRTCUDPPortMax, "--"+flagWebRTCUDPPortMax, err)
		}
		if min > max {
			return Config{}, fmt.Errorf("WebRTC UDP port range min (%d) must be <= max (%d)", min, max)
		}
		size := int(max) - int(min) + 1
		if size < recommendedWebRTCUDPPortRangeSize {
			return Config{}, fmt.Errorf("WebRTC UDP port range is too small: %d ports (min %d recommended)", size, recommendedWebRTCUDPPortRangeSize)
		}
		webrtcUDPPortRange = &UDPPortRange{Min: min, Max: max}
	}

	webrtcUDPListenIP := net.ParseIP(strings.TrimSpace(webrtcUDPListenIPStr))
	if webrtcUDPListenIP == nil {
		return Config{}, fmt.Errorf("invalid %s/%s %q", envVarWebRTCUDPListenIP, "--"+flagWebRTCUDPListenIP, webrtcUDPListenIPStr)
	}

	var webrtcNAT1To1IPs []string
	if strings.TrimSpace(webrtcNAT1To1IPsStr) != "" {
		ips, err := parseIPList(webrtcNAT1To1IPsStr)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s/%s %q: %w", envVarWebRTCNAT1To1IPs, "--"+flagWebRTCNAT1To1IPs, webrtcNAT1To1IPsStr, err)
		}
		webrtcNAT1To1IPs = ips
	}

	if strings.TrimSpace(webrtcNAT1To1CandidateTypeStr) == "" {
		webrtcNAT1To1CandidateTypeStr = string(NAT1To1CandidateTypeHost)
	}
	webrtcNAT1To1CandidateType, err := parseCandidateType(webrtcNAT1To1CandidateTypeStr)
	if err != nil {
		return Config{}, fmt.Errorf("invalid %s/%s %q: %w", envVarWebRTCNAT1To1IPCandidateType, "--"+flagWebRTCNAT1To1IPCandidateType, webrtcNAT1To1CandidateTypeStr, err)
	}

	allowedOrigins, err := parseAllowedOrigins(allowedOriginsStr)
	if err != nil {
		return Config{}, fmt.Errorf("%s/%s: %w", envVarAllowedOrigins, "--allowed-origins", err)
	}

	if strings.TrimSpace(l2BackendWSURL) != "" {
		u, err := url.Parse(strings.TrimSpace(l2BackendWSURL))
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s/%s %q: %w", envVarL2BackendWSURL, "--l2-backend-ws-url", l2BackendWSURL, err)
		}
		scheme := strings.ToLower(u.Scheme)
		if scheme != "ws" && scheme != "wss" {
			return Config{}, fmt.Errorf("invalid %s/%s %q (expected ws:// or wss://)", envVarL2BackendWSURL, "--l2-backend-ws-url", l2BackendWSURL)
		}
		if u.Host == "" {
			return Config{}, fmt.Errorf("invalid %s/%s %q (missing host)", envVarL2BackendWSURL, "--l2-backend-ws-url", l2BackendWSURL)
		}
		if u.User != nil {
			return Config{}, fmt.Errorf("invalid %s/%s %q (must not include credentials)", envVarL2BackendWSURL, "--l2-backend-ws-url", l2BackendWSURL)
		}
		// Preserve the original string (including path/query) but ensure whitespace
		// isn't part of the configured URL.
		l2BackendWSURL = strings.TrimSpace(l2BackendWSURL)

		// If an explicit origin override is set (via L2_BACKEND_ORIGIN or
		// L2_BACKEND_ORIGIN_OVERRIDE), it takes precedence over L2_BACKEND_WS_ORIGIN.
		// Avoid rejecting startup due to an invalid *unused* L2_BACKEND_WS_ORIGIN.
		if strings.TrimSpace(l2BackendWSOrigin) != "" && strings.TrimSpace(l2BackendOriginOverride) == "" {
			origin, err := normalizeOriginHeaderValue(l2BackendWSOrigin)
			if err != nil {
				return Config{}, fmt.Errorf("invalid %s/%s %q: %w", envVarL2BackendWSOrigin, "--l2-backend-ws-origin", l2BackendWSOrigin, err)
			}
			l2BackendWSOrigin = origin
		}

		if strings.TrimSpace(l2BackendWSToken) != "" {
			token := strings.TrimSpace(l2BackendWSToken)
			if !isValidWebSocketSubprotocolToken(token) {
				return Config{}, fmt.Errorf("invalid %s/%s: token must be a valid WebSocket subprotocol token (RFC 7230 tchar); got %q", envVarL2BackendWSToken, "--l2-backend-ws-token", l2BackendWSToken)
			}
			l2BackendWSToken = token
		}
	}

	if strings.TrimSpace(l2BackendWSToken) != "" {
		l2BackendWSToken = strings.TrimSpace(l2BackendWSToken)
		// RFC 6455: Sec-WebSocket-Protocol values must be HTTP "tokens".
		if !isHTTPToken(l2BackendWSToken) {
			return Config{}, fmt.Errorf("invalid %s/%s (token is not valid for Sec-WebSocket-Protocol)", envVarL2BackendWSToken, "--l2-backend-ws-token")
		}
	}

	if !envForwardOriginSet && !setFlags["l2-backend-forward-origin"] && strings.TrimSpace(l2BackendWSURL) != "" {
		// Default to forwarding Origin when L2 is enabled.
		l2BackendForwardOrigin = true
	}

	if strings.TrimSpace(l2BackendOriginOverride) != "" {
		origin, err := normalizeOriginValue(l2BackendOriginOverride)
		if err != nil {
			return Config{}, fmt.Errorf("invalid %s/%s %q: %w", envVarL2BackendOriginOverride, "--l2-backend-origin-override", l2BackendOriginOverride, err)
		}
		// Treat L2_BACKEND_ORIGIN_OVERRIDE/--l2-backend-origin-override as an alias
		// that overrides the backend Origin header configured via L2_BACKEND_WS_ORIGIN.
		l2BackendWSOrigin = origin
	}

	cfg := Config{
		ListenAddr:                  listenAddr,
		PublicBaseURL:               publicBaseURL,
		AllowedOrigins:              allowedOrigins,
		LogFormat:                   logFormat,
		LogLevel:                    level,
		ShutdownTimeout:             shutdownTimeout,
		ICEGatheringTimeout:         iceGatherTimeout,
		WebRTCSessionConnectTimeout: webrtcSessionConnectTimeout,
		Mode:                        mode,

		AuthMode:                      authMode,
		APIKey:                        apiKey,
		JWTSecret:                     jwtSecret,
		SignalingAuthTimeout:          signalingAuthTimeout,
		SignalingWSIdleTimeout:        signalingWSIdleTimeout,
		SignalingWSPingInterval:       signalingWSPingInterval,
		MaxSignalingMessageBytes:      maxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond: maxSignalingMessagesPerSecond,
		UDPWSIdleTimeout:              udpWSIdleTimeout,
		UDPWSPingInterval:             udpWSPingInterval,

		UDPBindingIdleTimeout:         udpBindingIdleTimeout,
		UDPInboundFilterMode:          udpInboundFilterMode,
		UDPRemoteAllowlistIdleTimeout: udpRemoteAllowlistIdleTimeout,
		UDPReadBufferBytes:            udpReadBufferBytes,
		DataChannelSendQueueBytes:     dataChannelSendQueueBytes,
		MaxDatagramPayloadBytes:       maxDatagramPayloadBytes,
		MaxAllowedRemotesPerBinding:   maxAllowedRemotesPerBinding,
		PreferV2:                      preferV2,

		L2BackendWSURL:              l2BackendWSURL,
		L2BackendWSOrigin:           l2BackendWSOrigin,
		L2BackendWSToken:            l2BackendWSToken,
		L2BackendForwardOrigin:      l2BackendForwardOrigin,
		L2BackendAuthForwardMode:    l2BackendAuthForwardMode,
		L2BackendForwardAeroSession: l2BackendForwardAeroSession,
		L2MaxMessageBytes:           l2MaxMessageBytes,

		WebRTCUDPPortRange:               webrtcUDPPortRange,
		WebRTCUDPListenIP:                webrtcUDPListenIP,
		WebRTCNAT1To1IPs:                 webrtcNAT1To1IPs,
		WebRTCNAT1To1IPCandidateType:     webrtcNAT1To1CandidateType,
		WebRTCDataChannelMaxMessageBytes: effectiveDCMax,
		WebRTCSCTPMaxReceiveBufferBytes:  effectiveSCTPRecvBuf,

		MaxSessions:                     maxSessions,
		SessionPreallocTTL:              sessionPreallocTTL,
		MaxUDPPpsPerSession:             maxUDPPpsPerSession,
		MaxUDPBpsPerSession:             maxUDPBpsPerSession,
		MaxUDPPpsPerDest:                maxUDPPpsPerDest,
		MaxUDPBindingsPerSession:        maxUDPBindingsPerSession,
		MaxUniqueDestinationsPerSession: maxUniqueDestinationsPerSession,
		MaxUDPDestBucketsPerSession:     maxUDPDestBucketsPerSession,
		MaxDataChannelBpsPerSession:     maxDataChannelBpsPerSession,
		HardCloseAfterViolations:        hardCloseAfterViolations,
		ViolationWindow:                 violationWindow,
		TURNREST: TurnRESTConfig{
			SharedSecret:   turnRESTSharedSecret,
			TTLSeconds:     turnRESTTTLSeconds,
			UsernamePrefix: turnRESTUsernamePrefix,
			Realm:          turnRESTRealm,
		},
	}

	iceServers, err := parseICEServersFromValues(
		iceServersJSON,
		stunURLs,
		turnURLs,
		turnUsername,
		turnCredential,
		cfg.TURNREST.Enabled(),
	)
	if err != nil {
		cfg.iceConfigErr = err
	} else {
		cfg.ICEServers = iceServers
	}

	return cfg, nil
}

func isHTTPToken(s string) bool {
	if s == "" {
		return false
	}
	for _, r := range s {
		if !isHTTPTokenChar(r) {
			return false
		}
	}
	return true
}

func isHTTPTokenChar(r rune) bool {
	if r >= '0' && r <= '9' {
		return true
	}
	if r >= 'A' && r <= 'Z' {
		return true
	}
	if r >= 'a' && r <= 'z' {
		return true
	}
	switch r {
	case '!', '#', '$', '%', '&', '\'', '*', '+', '-', '.', '^', '_', '`', '|', '~':
		return true
	default:
		return false
	}
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

func parseAuthMode(raw string) (AuthMode, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case string(AuthModeNone):
		return AuthModeNone, nil
	case string(AuthModeAPIKey):
		return AuthModeAPIKey, nil
	case string(AuthModeJWT):
		return AuthModeJWT, nil
	default:
		return "", fmt.Errorf("invalid %s %q (expected %s, %s, or %s)", envVarAuthMode, raw, AuthModeNone, AuthModeAPIKey, AuthModeJWT)
	}
}

func parseL2BackendAuthForwardMode(raw string) (L2BackendAuthForwardMode, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case string(L2BackendAuthForwardModeNone):
		return L2BackendAuthForwardModeNone, nil
	case string(L2BackendAuthForwardModeQuery), "":
		return L2BackendAuthForwardModeQuery, nil
	case string(L2BackendAuthForwardModeSubprotocol):
		return L2BackendAuthForwardModeSubprotocol, nil
	default:
		return "", fmt.Errorf("invalid %s %q (expected %s, %s, or %s)", envVarL2BackendAuthForwardMode, raw,
			L2BackendAuthForwardModeNone,
			L2BackendAuthForwardModeQuery,
			L2BackendAuthForwardModeSubprotocol,
		)
	}
}

func IsUnspecifiedIP(ip net.IP) bool {
	return ip == nil || ip.Equal(net.IPv4zero) || ip.Equal(net.IPv6zero)
}

func normalizeOriginValue(raw string) (string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", nil
	}
	if raw == "null" {
		return "null", nil
	}

	normalized, _, ok := origin.NormalizeHeader(raw)
	if !ok {
		return "", fmt.Errorf("expected full origin like https://example.com")
	}
	return normalized, nil
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

		if entry == "*" {
			out = append(out, entry)
			continue
		}

		normalizedOrigin, _, ok := origin.NormalizeHeader(entry)
		if !ok {
			return nil, fmt.Errorf("invalid origin %q (expected full origin like https://example.com)", entry)
		}
		out = append(out, normalizedOrigin)
	}

	return out, nil
}

func normalizeOriginHeaderValue(raw string) (string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", nil
	}
	if raw == "null" {
		return raw, nil
	}

	normalized, _, ok := origin.NormalizeHeader(raw)
	if !ok {
		return "", fmt.Errorf("expected full origin like https://example.com")
	}
	return normalized, nil
}

// isValidWebSocketSubprotocolToken reports whether raw is a valid WebSocket
// subprotocol token per RFC 6455, which uses the HTTP token grammar (RFC 7230
// tchar).
func isValidWebSocketSubprotocolToken(raw string) bool {
	if raw == "" {
		return false
	}
	for i := 0; i < len(raw); i++ {
		c := raw[i]
		switch {
		case c >= 'a' && c <= 'z':
			continue
		case c >= 'A' && c <= 'Z':
			continue
		case c >= '0' && c <= '9':
			continue
		}
		switch c {
		case '!', '#', '$', '%', '&', '\'', '*', '+', '-', '.', '^', '_', '`', '|', '~':
			continue
		default:
			return false
		}
	}
	return true
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

func parseUDPInboundFilterMode(s string) (UDPInboundFilterMode, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case string(UDPInboundFilterModeAny):
		return UDPInboundFilterModeAny, nil
	case string(UDPInboundFilterModeAddressAndPort):
		return UDPInboundFilterModeAddressAndPort, nil
	default:
		return "", fmt.Errorf("expected %s or %s", UDPInboundFilterModeAny, UDPInboundFilterModeAddressAndPort)
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

func iceServerHasTURNURL(server webrtc.ICEServer) bool {
	for _, raw := range server.URLs {
		url := strings.ToLower(strings.TrimSpace(raw))
		if strings.HasPrefix(url, "turn:") || strings.HasPrefix(url, "turns:") {
			return true
		}
	}
	return false
}
