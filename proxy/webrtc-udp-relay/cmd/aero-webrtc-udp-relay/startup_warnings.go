package main

import (
	"log/slog"
	"net/url"
	"strings"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
)

func logStartupSecurityWarnings(logger *slog.Logger, cfg config.Config, destPolicy *policy.DestinationPolicy) {
	if logger == nil {
		logger = slog.Default()
	}

	if cfg.AuthMode == config.AuthModeNone {
		logger.Warn("startup security warning: AUTH_MODE=none disables authentication",
			"warning_code", "auth_mode_none",
			"auth_mode", cfg.AuthMode,
			"mode", cfg.Mode,
		)
	}

	if containsString(cfg.AllowedOrigins, "*") {
		logger.Warn("startup security warning: ALLOWED_ORIGINS contains '*' (allows any origin)",
			"warning_code", "allowed_origins_wildcard",
			"allowed_origins", cfg.AllowedOrigins,
			"mode", cfg.Mode,
		)
	}

	if destPolicy != nil {
		// Note: warn on dev preset regardless of --mode since it is broadly permissive.
		if strings.EqualFold(destPolicy.Preset, "dev") {
			logger.Warn("startup security warning: destination policy preset is dev (allows private network UDP destinations)",
				"warning_code", "destination_policy_preset_dev",
				"destination_policy_preset", destPolicy.Preset,
				"allow_private_networks", destPolicy.AllowPrivateNetworks,
				"default_allow", destPolicy.DefaultAllow,
				"mode", cfg.Mode,
			)
		} else if cfg.Mode == config.ModeProd && destPolicy.AllowPrivateNetworks {
			logger.Warn("startup security warning: ALLOW_PRIVATE_NETWORKS=true while --mode=prod",
				"warning_code", "allow_private_networks_in_prod",
				"destination_policy_preset", destPolicy.Preset,
				"allow_private_networks", destPolicy.AllowPrivateNetworks,
				"default_allow", destPolicy.DefaultAllow,
				"mode", cfg.Mode,
			)
		}
	}

	if cfg.Mode == config.ModeProd && cfg.MaxSessions <= 0 {
		logger.Warn("startup security warning: MAX_SESSIONS is unset/0 (unlimited) while --mode=prod",
			"warning_code", "max_sessions_unlimited_in_prod",
			"max_sessions", cfg.MaxSessions,
			"mode", cfg.Mode,
		)
	}

	if cfg.L2BackendAuthForwardMode == config.L2BackendAuthForwardModeQuery &&
		cfg.AuthMode != config.AuthModeNone {
		logger.Warn("startup security warning: L2_BACKEND_AUTH_FORWARD_MODE=query may leak credentials via URLs when L2 bridging is enabled (prefer subprotocol)",
			"warning_code", "l2_backend_auth_forward_mode_query",
			"l2_backend_ws_url_set", strings.TrimSpace(cfg.L2BackendWSURL) != "",
			"l2_backend_ws_host", safeURLHost(cfg.L2BackendWSURL),
			"l2_backend_auth_forward_mode", cfg.L2BackendAuthForwardMode,
			"auth_mode", cfg.AuthMode,
			"mode", cfg.Mode,
		)
	}
}

func containsString(xs []string, v string) bool {
	for _, s := range xs {
		if s == v {
			return true
		}
	}
	return false
}

func safeURLHost(raw string) string {
	u, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return ""
	}
	return u.Host
}
