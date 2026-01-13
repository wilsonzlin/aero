package relay

import (
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

// InboundFilterMode controls which remote endpoints are allowed to send UDP
// packets back to a binding.
type InboundFilterMode int

const (
	// InboundFilterAny behaves like a full-cone NAT: inbound packets are accepted
	// from any remote endpoint.
	InboundFilterAny InboundFilterMode = iota
	// InboundFilterAddressAndPort behaves like a typical symmetric NAT: inbound
	// packets are accepted only from remote endpoints that the guest has
	// previously sent a packet to (address+port tuple).
	InboundFilterAddressAndPort
)

type Config struct {
	MaxUDPBindingsPerSession  int
	UDPBindingIdleTimeout     time.Duration
	UDPReadBufferBytes        int
	DataChannelSendQueueBytes int

	// MaxDatagramPayloadBytes is enforced on inbound client->server WebRTC frames.
	MaxDatagramPayloadBytes int

	// L2BackendWSURL configures the WebSocket endpoint (typically aero-l2-proxy)
	// used to bridge an L2 tunnel DataChannel labeled "l2".
	//
	// When empty, "l2" DataChannels are rejected.
	L2BackendWSURL string

	// L2BackendWSOrigin, when non-empty, is sent as the Origin header when the
	// relay dials the backend WebSocket.
	L2BackendWSOrigin string

	// L2BackendWSToken is an optional token presented to the L2 backend via an
	// additional offered WebSocket subprotocol entry (`aero-l2-token.<token>`).
	//
	// The negotiated subprotocol is still required to be `aero-l2-tunnel-v1`.
	L2BackendWSToken string

	// L2BackendForwardOrigin controls whether the relay forwards an Origin header
	// from the client signaling request to the L2 backend WebSocket dial.
	L2BackendForwardOrigin bool

	// L2BackendAuthForwardMode controls how the relay forwards the client's auth
	// credential when dialing the L2 backend WebSocket.
	L2BackendAuthForwardMode config.L2BackendAuthForwardMode

	// L2BackendForwardAeroSession controls whether the relay forwards the
	// caller's `aero_session` cookie to the L2 backend WebSocket.
	//
	// When enabled and the caller supplied the cookie during signaling, the relay
	// dials the backend with `Cookie: aero_session=<value>`. No other cookies are
	// forwarded.
	L2BackendForwardAeroSession bool

	// L2MaxMessageBytes bounds the size of individual L2 tunnel messages forwarded
	// over the "l2" DataChannel and backend WebSocket.
	L2MaxMessageBytes int

	// PreferV2 controls the encoding used for relay->client frames when the client
	// has demonstrated v2 support.
	//
	// v2 is always used for IPv6 packets (v1 cannot represent IPv6).
	PreferV2 bool

	InboundFilterMode InboundFilterMode

	// RemoteAllowlistIdleTimeout expires allowlist entries. If zero, defaults to
	// UDPBindingIdleTimeout.
	RemoteAllowlistIdleTimeout time.Duration

	// MaxAllowedRemotesPerBinding caps the size of the per-binding remote allowlist
	// used by inbound filtering (InboundFilterAddressAndPort).
	//
	// This is defense-in-depth against a guest spraying UDP packets to many
	// destinations on the same guest port.
	MaxAllowedRemotesPerBinding int
}

func DefaultConfig() Config {
	return Config{
		MaxUDPBindingsPerSession:  128,
		UDPBindingIdleTimeout:     60 * time.Second,
		DataChannelSendQueueBytes: 1 << 20, // 1MiB
		MaxDatagramPayloadBytes:   udpproto.DefaultMaxPayload,
		// Allocate only enough to detect oversized payloads (Max+1), rather than a
		// full 64KiB UDP payload per binding.
		UDPReadBufferBytes:        udpproto.DefaultMaxPayload + 1,
		L2BackendAuthForwardMode:  config.L2BackendAuthForwardModeQuery,
		L2MaxMessageBytes:         4096,
		InboundFilterMode:         InboundFilterAddressAndPort,
		MaxAllowedRemotesPerBinding: 1024,
	}
}

func (c Config) withDefaults() Config {
	d := DefaultConfig()
	if c.MaxUDPBindingsPerSession <= 0 {
		c.MaxUDPBindingsPerSession = d.MaxUDPBindingsPerSession
	}
	if c.UDPBindingIdleTimeout <= 0 {
		c.UDPBindingIdleTimeout = d.UDPBindingIdleTimeout
	}
	if c.DataChannelSendQueueBytes <= 0 {
		c.DataChannelSendQueueBytes = d.DataChannelSendQueueBytes
	}
	if c.MaxDatagramPayloadBytes <= 0 {
		c.MaxDatagramPayloadBytes = d.MaxDatagramPayloadBytes
	}
	// Default the UDP read buffer to MaxDatagramPayloadBytes+1 so we can detect
	// oversized payloads without allocating ~64KiB per binding.
	if c.UDPReadBufferBytes <= 0 {
		c.UDPReadBufferBytes = c.MaxDatagramPayloadBytes + 1
	}
	if c.L2MaxMessageBytes <= 0 {
		c.L2MaxMessageBytes = d.L2MaxMessageBytes
	}
	if c.L2BackendAuthForwardMode == "" {
		c.L2BackendAuthForwardMode = d.L2BackendAuthForwardMode
	}
	if c.RemoteAllowlistIdleTimeout <= 0 {
		c.RemoteAllowlistIdleTimeout = c.UDPBindingIdleTimeout
	}
	if c.MaxAllowedRemotesPerBinding <= 0 {
		c.MaxAllowedRemotesPerBinding = d.MaxAllowedRemotesPerBinding
	}
	return c
}

// WithDefaults returns c with any zero/invalid fields replaced with sensible
// defaults.
func (c Config) WithDefaults() Config {
	return c.withDefaults()
}
