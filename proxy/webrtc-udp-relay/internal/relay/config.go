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
//   - MAX_UDP_BINDINGS_PER_SESSION (int)
//   - UDP_BINDING_IDLE_TIMEOUT (duration, e.g. 60s)
//   - UDP_READ_BUFFER_BYTES (int)
//   - DATACHANNEL_SEND_QUEUE_BYTES (int)
func ConfigFromEnv() Config {
	c := DefaultConfig()
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
