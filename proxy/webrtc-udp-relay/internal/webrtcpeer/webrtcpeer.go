package webrtcpeer

import (
	"fmt"
	"net"
	"reflect"
	"strconv"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func NewAPI(cfg config.Config) (*webrtc.API, error) {
	se := webrtc.SettingEngine{}
	if err := applyNetworkSettings(&se, cfg); err != nil {
		return nil, err
	}

	mediaEngine := &webrtc.MediaEngine{}
	if err := mediaEngine.RegisterDefaultCodecs(); err != nil {
		return nil, fmt.Errorf("register default codecs: %w", err)
	}

	api := webrtc.NewAPI(
		webrtc.WithSettingEngine(se),
		webrtc.WithMediaEngine(mediaEngine),
	)
	return api, nil
}

func applyNetworkSettings(se *webrtc.SettingEngine, cfg config.Config) error {
	if cfg.WebRTCUDPPortRange != nil {
		if err := se.SetEphemeralUDPPortRange(cfg.WebRTCUDPPortRange.Min, cfg.WebRTCUDPPortRange.Max); err != nil {
			return fmt.Errorf("set ephemeral udp port range: %w", err)
		}
	}

	if len(cfg.WebRTCNAT1To1IPs) > 0 {
		var candidateType webrtc.ICECandidateType
		switch cfg.WebRTCNAT1To1IPCandidateType {
		case config.NAT1To1CandidateTypeHost:
			candidateType = webrtc.ICECandidateTypeHost
		case config.NAT1To1CandidateTypeSrflx:
			candidateType = webrtc.ICECandidateTypeSrflx
		default:
			return fmt.Errorf("invalid NAT 1:1 IP candidate type %q", cfg.WebRTCNAT1To1IPCandidateType)
		}
		se.SetNAT1To1IPs(cfg.WebRTCNAT1To1IPs, candidateType)
	}

	// SettingEngine doesn't currently expose a "bind to 0.0.0.0" toggle; instead
	// we restrict candidate gathering and socket binding via IPFilter.
	if !config.IsUnspecifiedIP(cfg.WebRTCUDPListenIP) {
		listenIP := cfg.WebRTCUDPListenIP
		se.SetIPFilter(func(ip net.IP) bool {
			return ip.Equal(listenIP)
		})
	}

	// WebRTC DataChannel hardening (DoS mitigation).
	//
	// The relay enforces MAX_DATAGRAM_PAYLOAD_BYTES and L2_MAX_MESSAGE_BYTES at
	// the application layer, but a malicious peer can attempt to send extremely
	// large SCTP messages that pion would otherwise allocate before OnMessage is
	// invoked.
	//
	// Note: In pion/webrtc, `SetSCTPMaxMessageSize` controls the `a=max-message-size`
	// value advertised in SDP. This helps well-behaved peers avoid sending
	// oversized user messages, but malicious peers can ignore SDP negotiation.
	//
	// The receive-side hard cap that bounds buffering/allocation before
	// DataChannel.OnMessage runs is `SetSCTPMaxReceiveBufferSize`.
	if cfg.WebRTCDataChannelMaxMessageBytes > 0 {
		v, err := asUint32(cfg.WebRTCDataChannelMaxMessageBytes)
		if err != nil {
			return fmt.Errorf("invalid WebRTCDataChannelMaxMessageBytes=%d: %w", cfg.WebRTCDataChannelMaxMessageBytes, err)
		}
		if err := setSettingEngineUint(se, []string{"SetSCTPMaxMessageSize"}, v); err != nil {
			return fmt.Errorf("set SCTP max message size: %w", err)
		}
	}
	if cfg.WebRTCSCTPMaxReceiveBufferBytes > 0 {
		v, err := asUint32(cfg.WebRTCSCTPMaxReceiveBufferBytes)
		if err != nil {
			return fmt.Errorf("invalid WebRTCSCTPMaxReceiveBufferBytes=%d: %w", cfg.WebRTCSCTPMaxReceiveBufferBytes, err)
		}
		if err := setSettingEngineUint(se, []string{"SetSCTPMaxReceiveBufferSize", "SetSCTPReceiveBufferSize"}, v); err != nil {
			return fmt.Errorf("set SCTP max receive buffer size: %w", err)
		}
	}

	return nil
}

func asUint32(v int) (uint32, error) {
	if v < 0 {
		return 0, fmt.Errorf("must be >= 0")
	}
	if uint64(v) > uint64(^uint32(0)) {
		return 0, fmt.Errorf("must be <= %s", strconv.FormatUint(uint64(^uint32(0)), 10))
	}
	return uint32(v), nil
}

func setSettingEngineUint(se *webrtc.SettingEngine, methodNames []string, v uint32) error {
	if se == nil {
		return fmt.Errorf("nil SettingEngine")
	}
	seVal := reflect.ValueOf(se)
	for _, name := range methodNames {
		m := seVal.MethodByName(name)
		if !m.IsValid() {
			continue
		}

		mt := m.Type()
		if mt.NumIn() != 1 {
			return fmt.Errorf("SettingEngine.%s has unexpected signature: wants %d args", name, mt.NumIn())
		}

		argT := mt.In(0)
		var arg reflect.Value
		switch argT.Kind() {
		case reflect.Uint32:
			arg = reflect.ValueOf(v)
		case reflect.Uint64:
			arg = reflect.ValueOf(uint64(v))
		case reflect.Uint:
			arg = reflect.ValueOf(uint(v))
		case reflect.Int:
			arg = reflect.ValueOf(int(v))
		case reflect.Int64:
			arg = reflect.ValueOf(int64(v))
		default:
			return fmt.Errorf("SettingEngine.%s has unsupported arg type %s", name, argT.String())
		}

		out := m.Call([]reflect.Value{arg})
		switch len(out) {
		case 0:
			return nil
		case 1:
			if err, ok := out[0].Interface().(error); ok {
				return err
			}
			return fmt.Errorf("SettingEngine.%s returned non-error type %s", name, mt.Out(0).String())
		default:
			return fmt.Errorf("SettingEngine.%s returned %d values (expected 0 or 1)", name, len(out))
		}
	}
	return fmt.Errorf("SettingEngine missing method(s) %v", methodNames)
}

// configuredSCTPMaxMessageSize returns the SettingEngine-configured SCTP max
// message size for an API created via webrtc.NewAPI.
//
// Pion does not currently expose this value directly on *webrtc.API. We use
// reflection so other parts of the relay can enforce this as an application
// layer guardrail (e.g. close sessions that receive oversized DataChannel
// messages).
func configuredSCTPMaxMessageSize(api *webrtc.API) int {
	if api == nil {
		return 0
	}
	// API is a concrete struct type; reflect on the pointer.
	apiV := reflect.ValueOf(api)
	if apiV.Kind() != reflect.Ptr || apiV.IsNil() {
		return 0
	}
	apiV = apiV.Elem()
	if apiV.Kind() != reflect.Struct {
		return 0
	}

	seV := apiV.FieldByName("settingEngine")
	if !seV.IsValid() || seV.Kind() != reflect.Ptr || seV.IsNil() {
		return 0
	}
	seV = seV.Elem()
	if seV.Kind() != reflect.Struct {
		return 0
	}

	sctpV := seV.FieldByName("sctp")
	if !sctpV.IsValid() || sctpV.Kind() != reflect.Struct {
		return 0
	}
	maxV := sctpV.FieldByName("maxMessageSize")
	if !maxV.IsValid() {
		return 0
	}

	switch maxV.Kind() {
	case reflect.Uint, reflect.Uint32, reflect.Uint64, reflect.Uint16, reflect.Uint8:
		return int(maxV.Uint())
	case reflect.Int, reflect.Int32, reflect.Int64, reflect.Int16, reflect.Int8:
		return int(maxV.Int())
	default:
		return 0
	}
}
