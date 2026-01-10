package webrtcpeer

import (
	"fmt"
	"net"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func NewAPI(cfg config.Config) (*webrtc.API, error) {
	se := webrtc.SettingEngine{}
	if err := ApplyNetworkSettings(&se, cfg); err != nil {
		return nil, err
	}
	api := webrtc.NewAPI(webrtc.WithSettingEngine(se))
	return api, nil
}

func ApplyNetworkSettings(se *webrtc.SettingEngine, cfg config.Config) error {
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

	return nil
}
