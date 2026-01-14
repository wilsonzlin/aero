package signaling

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"

	"github.com/pion/webrtc/v4"
)

type messageType string

const (
	messageTypeAuth      messageType = "auth"
	messageTypeOffer     messageType = "offer"
	messageTypeAnswer    messageType = "answer"
	messageTypeCandidate messageType = "candidate"
	messageTypeClose     messageType = "close"
	messageTypeError     messageType = "error"
)

type sdp struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

func sdpFromPion(desc webrtc.SessionDescription) sdp {
	return sdp{
		Type: desc.Type.String(),
		SDP:  desc.SDP,
	}
}

func (s sdp) ToPion() (webrtc.SessionDescription, error) {
	var t webrtc.SDPType
	switch s.Type {
	case "offer":
		t = webrtc.SDPTypeOffer
	case "answer":
		t = webrtc.SDPTypeAnswer
	default:
		return webrtc.SessionDescription{}, fmt.Errorf("unsupported sdp type %q", s.Type)
	}
	return webrtc.SessionDescription{Type: t, SDP: s.SDP}, nil
}

type candidate struct {
	Candidate        string  `json:"candidate"`
	SDPMid           *string `json:"sdpMid,omitempty"`
	SDPMLineIndex    *uint16 `json:"sdpMLineIndex,omitempty"`
	UsernameFragment *string `json:"usernameFragment,omitempty"`
}

func candidateFromPion(init webrtc.ICECandidateInit) candidate {
	return candidate{
		Candidate:        init.Candidate,
		SDPMid:           init.SDPMid,
		SDPMLineIndex:    init.SDPMLineIndex,
		UsernameFragment: init.UsernameFragment,
	}
}

func (c candidate) ToPion() webrtc.ICECandidateInit {
	return webrtc.ICECandidateInit{
		Candidate:        c.Candidate,
		SDPMid:           c.SDPMid,
		SDPMLineIndex:    c.SDPMLineIndex,
		UsernameFragment: c.UsernameFragment,
	}
}

type signalMessage struct {
	Type      messageType `json:"type"`
	SDP       *sdp        `json:"sdp,omitempty"`
	Candidate *candidate  `json:"candidate,omitempty"`

	APIKey string `json:"apiKey,omitempty"`
	Token  string `json:"token,omitempty"`

	Code    string `json:"code,omitempty"`
	Message string `json:"message,omitempty"`
}

func parseSignalMessage(data []byte) (signalMessage, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()

	var msg signalMessage
	if err := dec.Decode(&msg); err != nil {
		return signalMessage{}, err
	}
	if err := msg.validate(); err != nil {
		return signalMessage{}, err
	}
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return signalMessage{}, fmt.Errorf("unexpected trailing data")
	}
	return msg, nil
}

func (m signalMessage) validate() error {
	switch m.Type {
	case messageTypeAuth:
		if m.APIKey == "" && m.Token == "" {
			return fmt.Errorf("auth message missing apiKey/token")
		}
		if m.APIKey != "" && m.Token != "" && m.APIKey != m.Token {
			return fmt.Errorf("auth message must not include both apiKey and token unless they match")
		}
		if m.SDP != nil || m.Candidate != nil || m.Code != "" || m.Message != "" {
			return fmt.Errorf("auth message has unexpected fields")
		}
	case messageTypeOffer:
		if m.SDP == nil {
			return fmt.Errorf("offer message missing sdp")
		}
		if m.SDP.Type != "offer" {
			return fmt.Errorf("offer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("offer message has unexpected fields")
		}
	case messageTypeAnswer:
		if m.SDP == nil {
			return fmt.Errorf("answer message missing sdp")
		}
		if m.SDP.Type != "answer" {
			return fmt.Errorf("answer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("answer message has unexpected fields")
		}
	case messageTypeCandidate:
		if m.Candidate == nil {
			return fmt.Errorf("candidate message missing candidate")
		}
		if m.SDP != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("candidate message has unexpected fields")
		}
	case messageTypeClose:
		if m.SDP != nil || m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("close message has unexpected fields")
		}
	case messageTypeError:
		if m.Code == "" || m.Message == "" {
			return fmt.Errorf("error message missing code/message")
		}
		if m.SDP != nil || m.Candidate != nil || m.APIKey != "" || m.Token != "" {
			return fmt.Errorf("error message has unexpected fields")
		}
	default:
		return fmt.Errorf("unsupported message type %q", m.Type)
	}
	return nil
}
