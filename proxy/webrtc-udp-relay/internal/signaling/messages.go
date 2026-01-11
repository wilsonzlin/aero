package signaling

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"

	"github.com/pion/webrtc/v4"
)

type MessageType string

const (
	MessageTypeAuth      MessageType = "auth"
	MessageTypeOffer     MessageType = "offer"
	MessageTypeAnswer    MessageType = "answer"
	MessageTypeCandidate MessageType = "candidate"
	MessageTypeClose     MessageType = "close"
	MessageTypeError     MessageType = "error"
)

type SDP struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

func SDPFromPion(desc webrtc.SessionDescription) SDP {
	return SDP{
		Type: desc.Type.String(),
		SDP:  desc.SDP,
	}
}

func (s SDP) ToPion() (webrtc.SessionDescription, error) {
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

type Candidate struct {
	Candidate        string  `json:"candidate"`
	SDPMid           *string `json:"sdpMid,omitempty"`
	SDPMLineIndex    *uint16 `json:"sdpMLineIndex,omitempty"`
	UsernameFragment *string `json:"usernameFragment,omitempty"`
}

func CandidateFromPion(init webrtc.ICECandidateInit) Candidate {
	return Candidate{
		Candidate:        init.Candidate,
		SDPMid:           init.SDPMid,
		SDPMLineIndex:    init.SDPMLineIndex,
		UsernameFragment: init.UsernameFragment,
	}
}

func (c Candidate) ToPion() webrtc.ICECandidateInit {
	return webrtc.ICECandidateInit{
		Candidate:        c.Candidate,
		SDPMid:           c.SDPMid,
		SDPMLineIndex:    c.SDPMLineIndex,
		UsernameFragment: c.UsernameFragment,
	}
}

type SignalMessage struct {
	Type      MessageType `json:"type"`
	SDP       *SDP        `json:"sdp,omitempty"`
	Candidate *Candidate  `json:"candidate,omitempty"`

	APIKey string `json:"apiKey,omitempty"`
	Token  string `json:"token,omitempty"`

	Code    string `json:"code,omitempty"`
	Message string `json:"message,omitempty"`
}

func ParseSignalMessage(data []byte) (SignalMessage, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()

	var msg SignalMessage
	if err := dec.Decode(&msg); err != nil {
		return SignalMessage{}, err
	}
	if err := msg.Validate(); err != nil {
		return SignalMessage{}, err
	}
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return SignalMessage{}, fmt.Errorf("unexpected trailing data")
	}
	return msg, nil
}

func (m SignalMessage) Validate() error {
	switch m.Type {
	case MessageTypeAuth:
		if m.APIKey == "" && m.Token == "" {
			return fmt.Errorf("auth message missing apiKey/token")
		}
		if m.APIKey != "" && m.Token != "" && m.APIKey != m.Token {
			return fmt.Errorf("auth message must not include both apiKey and token unless they match")
		}
		if m.SDP != nil || m.Candidate != nil || m.Code != "" || m.Message != "" {
			return fmt.Errorf("auth message has unexpected fields")
		}
	case MessageTypeOffer:
		if m.SDP == nil {
			return fmt.Errorf("offer message missing sdp")
		}
		if m.SDP.Type != "offer" {
			return fmt.Errorf("offer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("offer message has unexpected fields")
		}
	case MessageTypeAnswer:
		if m.SDP == nil {
			return fmt.Errorf("answer message missing sdp")
		}
		if m.SDP.Type != "answer" {
			return fmt.Errorf("answer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("answer message has unexpected fields")
		}
	case MessageTypeCandidate:
		if m.Candidate == nil {
			return fmt.Errorf("candidate message missing candidate")
		}
		if m.SDP != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("candidate message has unexpected fields")
		}
	case MessageTypeClose:
		if m.SDP != nil || m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return fmt.Errorf("close message has unexpected fields")
		}
	case MessageTypeError:
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
