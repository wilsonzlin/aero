package relay

import (
	"os"
	"strconv"
	"time"

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
}

func DefaultConfig() Config {
	return Config{
		MaxUDPBindingsPerSession:  128,
		UDPBindingIdleTimeout:     60 * time.Second,
		UDPReadBufferBytes:        65535,
		DataChannelSendQueueBytes: 1 << 20, // 1MiB
		MaxDatagramPayloadBytes:   udpproto.DefaultMaxPayload,
		L2MaxMessageBytes:         4096,
		InboundFilterMode:         InboundFilterAddressAndPort,
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
	if c.UDPReadBufferBytes <= 0 {
		c.UDPReadBufferBytes = d.UDPReadBufferBytes
	}
	if c.DataChannelSendQueueBytes <= 0 {
		c.DataChannelSendQueueBytes = d.DataChannelSendQueueBytes
	}
	if c.MaxDatagramPayloadBytes <= 0 {
		c.MaxDatagramPayloadBytes = d.MaxDatagramPayloadBytes
	}
	if c.L2MaxMessageBytes <= 0 {
		c.L2MaxMessageBytes = d.L2MaxMessageBytes
	}
	if c.RemoteAllowlistIdleTimeout <= 0 {
		c.RemoteAllowlistIdleTimeout = c.UDPBindingIdleTimeout
	}
	return c
}

// WithDefaults returns c with any zero/invalid fields replaced with sensible
// defaults.
func (c Config) WithDefaults() Config {
	return c.withDefaults()
}

// ConfigFromEnv returns Config using defaults overridden by environment
// variables.
//
// Environment variables:
//   - PREFER_V2 (bool) - prefer v2 relay->client frames once the client has demonstrated v2 support
//   - MAX_DATAGRAM_PAYLOAD_BYTES (int)
//   - MAX_UDP_BINDINGS_PER_SESSION (int)
//   - UDP_BINDING_IDLE_TIMEOUT (duration, e.g. 60s)
//   - UDP_READ_BUFFER_BYTES (int)
//   - DATACHANNEL_SEND_QUEUE_BYTES (int)
func ConfigFromEnv() Config {
	c := DefaultConfig()
	c.PreferV2 = udpproto.PreferV2FromEnv()
	c.L2BackendWSURL = os.Getenv("L2_BACKEND_WS_URL")
	if v := os.Getenv("L2_MAX_MESSAGE_BYTES"); v != "" {
		if i, err := strconv.Atoi(v); err == nil && i > 0 {
			c.L2MaxMessageBytes = i
		}
	}
	if v := os.Getenv("MAX_DATAGRAM_PAYLOAD_BYTES"); v != "" {
		if i, err := strconv.Atoi(v); err == nil && i > 0 {
			c.MaxDatagramPayloadBytes = i
		}
	}
	if v := os.Getenv("MAX_UDP_BINDINGS_PER_SESSION"); v != "" {
		if i, err := strconv.Atoi(v); err == nil && i > 0 {
			c.MaxUDPBindingsPerSession = i
		}
	}
	if v := os.Getenv("UDP_BINDING_IDLE_TIMEOUT"); v != "" {
		if d, err := time.ParseDuration(v); err == nil && d > 0 {
			c.UDPBindingIdleTimeout = d
		}
	}
	if v := os.Getenv("UDP_READ_BUFFER_BYTES"); v != "" {
		if i, err := strconv.Atoi(v); err == nil && i > 0 {
			c.UDPReadBufferBytes = i
		}
	}
	if v := os.Getenv("DATACHANNEL_SEND_QUEUE_BYTES"); v != "" {
		if i, err := strconv.Atoi(v); err == nil && i > 0 {
			c.DataChannelSendQueueBytes = i
		}
	}
	return c
}
