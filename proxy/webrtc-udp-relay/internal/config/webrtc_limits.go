package config

import "github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"

// webrtcDataChannelUDPFrameOverheadBytes is the worst-case overhead (in bytes)
// for a single UDP relay DataChannel message on top of MAX_DATAGRAM_PAYLOAD_BYTES.
//
// This accounts for the v2 header carrying an IPv6 address (see udpproto.Codec.EncodeFrameV2).
const webrtcDataChannelUDPFrameOverheadBytes = udpproto.MaxFrameOverheadBytes

// minWebRTCSCTPReceiveBufferBytes is the minimum SCTP receive buffer size that
// pion/sctp will accept during association setup. Values below this break SCTP
// negotiation (INIT/INIT-ACK validation).
const minWebRTCSCTPReceiveBufferBytes = 1500

func minWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes int) int {
	if maxDatagramPayloadBytes < 0 {
		maxDatagramPayloadBytes = 0
	}
	if l2MaxMessageBytes < 0 {
		l2MaxMessageBytes = 0
	}

	udpFrameMax := maxDatagramPayloadBytes + webrtcDataChannelUDPFrameOverheadBytes
	if udpFrameMax < 0 {
		udpFrameMax = 0
	}
	if l2MaxMessageBytes > udpFrameMax {
		return l2MaxMessageBytes
	}
	return udpFrameMax
}

func defaultWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes int) int {
	min := minWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
	// Add a small allowance for future protocol overhead.
	max := min + DefaultWebRTCDataChannelMaxMessageOverheadBytes
	if max < min {
		// Overflow on 32-bit systems is not expected, but clamp defensively.
		return min
	}
	return max
}

func defaultWebRTCSCTPMaxReceiveBufferBytes(maxMessageBytes int) int {
	if maxMessageBytes < 0 {
		maxMessageBytes = 0
	}
	buf := DefaultWebRTCSCTPMaxReceiveBufferBytes

	// Keep the receive buffer comfortably above the per-message cap so that a
	// small amount of in-flight data does not immediately stall the association.
	if twice := maxMessageBytes * 2; twice > buf {
		buf = twice
	}
	if buf < maxMessageBytes {
		buf = maxMessageBytes
	}
	if buf < minWebRTCSCTPReceiveBufferBytes {
		buf = minWebRTCSCTPReceiveBufferBytes
	}
	return buf
}
