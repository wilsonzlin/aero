package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"runtime/debug"
	"syscall"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/webrtcpeer"
)

var (
	// Set via -ldflags at build time. Values may be empty in local/dev builds.
	buildCommit = ""
	buildTime   = ""
)

func main() {
	cfg, err := config.Load(os.Args[1:])
	if err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return
		}
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}

	logger, err := config.NewLogger(cfg)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	slog.SetDefault(logger)

	// Construct the WebRTC API early so misconfigurations are caught on startup.
	// This does not start any networking; ICE sockets are only created once we
	// start creating PeerConnections.
	api, err := webrtcpeer.NewAPI(cfg)
	if err != nil {
		logger.Error("failed to configure webrtc", "err", err)
		os.Exit(2)
	}

	logger.Info("starting aero-webrtc-udp-relay",
		"listen_addr", cfg.ListenAddr,
		"public_base_url", cfg.PublicBaseURL,
		"mode", cfg.Mode,
		"max_datagram_payload_bytes", cfg.MaxDatagramPayloadBytes,
		"l2_max_message_bytes", cfg.L2MaxMessageBytes,
		"webrtc_datachannel_max_message_bytes", cfg.WebRTCDataChannelMaxMessageBytes,
		"webrtc_sctp_max_receive_buffer_bytes", cfg.WebRTCSCTPMaxReceiveBufferBytes,
		"webrtc_session_connect_timeout", cfg.WebRTCSessionConnectTimeout,
		"udp_inbound_filter_mode", cfg.UDPInboundFilterMode,
		"max_sessions", cfg.MaxSessions,
		"prefer_v2", cfg.PreferV2,
		"l2_backend_ws_url_set", cfg.L2BackendWSURL != "",
		"l2_backend_ws_host", safeURLHost(cfg.L2BackendWSURL),
	)

	destPolicy, err := policy.NewDestinationPolicyFromEnv()
	if err != nil {
		logger.Error("failed to load destination policy", "err", err)
		os.Exit(2)
	}

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	ln, err := net.Listen("tcp", cfg.ListenAddr)
	if err != nil {
		logger.Error("failed to listen", "err", err)
		os.Exit(1)
	}

	commit, buildTime := resolveBuildInfo(buildCommit, buildTime)

	srv := httpserver.New(cfg, logger, commit, buildTime)
	sessionMgr := relay.NewSessionManager(cfg, nil, nil)
	srv.SetMetrics(sessionMgr.Metrics())
	authz, err := signaling.NewAuthAuthorizer(cfg)
	if err != nil {
		logger.Error("failed to configure signaling auth", "err", err)
		os.Exit(2)
	}
	relayCfg := relay.Config{
		MaxUDPBindingsPerSession:    cfg.MaxUDPBindingsPerSession,
		UDPBindingIdleTimeout:       cfg.UDPBindingIdleTimeout,
		UDPReadBufferBytes:          cfg.UDPReadBufferBytes,
		DataChannelSendQueueBytes:   cfg.DataChannelSendQueueBytes,
		MaxDatagramPayloadBytes:     cfg.MaxDatagramPayloadBytes,
		InboundFilterMode:           inboundFilterMode(cfg.UDPInboundFilterMode),
		RemoteAllowlistIdleTimeout:  cfg.UDPRemoteAllowlistIdleTimeout,
		MaxAllowedRemotesPerBinding: cfg.MaxAllowedRemotesPerBinding,
		L2BackendWSURL:              cfg.L2BackendWSURL,
		L2BackendWSOrigin:           cfg.L2BackendWSOrigin,
		L2BackendWSToken:            cfg.L2BackendWSToken,
		L2BackendForwardOrigin:      cfg.L2BackendForwardOrigin,
		L2BackendAuthForwardMode:    cfg.L2BackendAuthForwardMode,
		L2BackendForwardAeroSession: cfg.L2BackendForwardAeroSession,
		L2MaxMessageBytes:           cfg.L2MaxMessageBytes,
		PreferV2:                    cfg.PreferV2,
	}
	sig := signaling.NewServer(signaling.Config{
		Sessions:                         sessionMgr,
		WebRTC:                           api,
		ICEServers:                       cfg.PeerConnectionICEServers(),
		RelayConfig:                      relayCfg,
		Policy:                           destPolicy,
		WebRTCDataChannelMaxMessageBytes: cfg.WebRTCDataChannelMaxMessageBytes,
		AllowedOrigins:                   cfg.AllowedOrigins,
		Authorizer:                       authz,
		ICEGatheringTimeout:              cfg.ICEGatheringTimeout,
		WebRTCSessionConnectTimeout:      cfg.WebRTCSessionConnectTimeout,
		SessionPreallocTTL:               cfg.SessionPreallocTTL,

		SignalingAuthTimeout:          cfg.SignalingAuthTimeout,
		SignalingWSIdleTimeout:        cfg.SignalingWSIdleTimeout,
		SignalingWSPingInterval:       cfg.SignalingWSPingInterval,
		MaxSignalingMessageBytes:      cfg.MaxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond: cfg.MaxSignalingMessagesPerSecond,
	})
	sig.RegisterRoutes(srv.Mux())

	udpWS, err := relay.NewUDPWebSocketServer(cfg, sessionMgr, relayCfg, destPolicy, logger)
	if err != nil {
		logger.Error("failed to configure /udp websocket server", "err", err)
		os.Exit(2)
	}
	srv.Mux().Handle("GET /udp", udpWS)

	// Expose internal counters in Prometheus' text format.
	srv.Mux().Handle("GET /metrics", metrics.PrometheusHandler(sessionMgr.Metrics()))

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	select {
	case err := <-errCh:
		sig.Close()
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			logger.Error("http server exited", "err", err)
			os.Exit(1)
		}
		return
	case <-ctx.Done():
		logger.Info("shutdown signal received")
	}

	shutdownCtx, cancel := context.WithTimeout(context.Background(), cfg.ShutdownTimeout)
	defer cancel()

	if err := srv.Shutdown(shutdownCtx); err != nil {
		logger.Error("http server shutdown failed", "err", err)
	}
	sig.Close()

	if err := <-errCh; err != nil && !errors.Is(err, http.ErrServerClosed) {
		logger.Error("http server exited after shutdown", "err", err)
		os.Exit(1)
	}
}

func inboundFilterMode(mode config.UDPInboundFilterMode) relay.InboundFilterMode {
	switch mode {
	case config.UDPInboundFilterModeAny:
		return relay.InboundFilterAny
	case config.UDPInboundFilterModeAddressAndPort:
		return relay.InboundFilterAddressAndPort
	default:
		// Should be validated by config.Load.
		return relay.InboundFilterAddressAndPort
	}
}

func resolveBuildInfo(commit, buildTime string) (string, string) {
	// Prefer ldflags-injected values (production builds) but fall back to the Go
	// build info when available (useful for `go run` / dev builds).
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, s := range bi.Settings {
			switch s.Key {
			case "vcs.revision":
				if commit == "" {
					commit = s.Value
				}
			case "vcs.time":
				if buildTime == "" {
					buildTime = s.Value
				}
			}
		}
	}

	return commit, buildTime
}
