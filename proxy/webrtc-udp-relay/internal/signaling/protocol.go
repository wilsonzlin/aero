package signaling

import (
	"encoding/json"
	"errors"
	"fmt"
)

const (
	// Version1 is the current signaling schema version used by the relay.
	Version1 = 1
)

var (
	ErrUnsupportedVersion = errors.New("signaling: unsupported version")
	ErrInvalidSDPType     = errors.New("signaling: invalid session description type")
	ErrMissingSDP         = errors.New("signaling: missing session description sdp")
)

// SessionDescription is a minimal, JSON-friendly representation of an SDP offer/answer.
//
// We intentionally avoid depending on any WebRTC library type here; this package
// models the protocol surface, not the implementation.
type SessionDescription struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

// OfferRequest is sent by the browser/client to the relay.
type OfferRequest struct {
	Version int                `json:"version"`
	Offer   SessionDescription `json:"offer"`
}

// AnswerResponse is returned by the relay in response to an OfferRequest.
type AnswerResponse struct {
	Version int                `json:"version"`
	Answer  SessionDescription `json:"answer"`
}

func (r OfferRequest) Validate() error {
	if r.Version != Version1 {
		return fmt.Errorf("%w: %d", ErrUnsupportedVersion, r.Version)
	}
	if r.Offer.Type != "offer" {
		return fmt.Errorf("%w: %q", ErrInvalidSDPType, r.Offer.Type)
	}
	if r.Offer.SDP == "" {
		return ErrMissingSDP
	}
	return nil
}

func (r AnswerResponse) Validate() error {
	if r.Version != Version1 {
		return fmt.Errorf("%w: %d", ErrUnsupportedVersion, r.Version)
	}
	if r.Answer.Type != "answer" {
		return fmt.Errorf("%w: %q", ErrInvalidSDPType, r.Answer.Type)
	}
	if r.Answer.SDP == "" {
		return ErrMissingSDP
	}
	return nil
}

func ParseOfferRequestJSON(b []byte) (OfferRequest, error) {
	var r OfferRequest
	if err := json.Unmarshal(b, &r); err != nil {
		return OfferRequest{}, err
	}
	if err := r.Validate(); err != nil {
		return OfferRequest{}, err
	}
	return r, nil
}

func ParseAnswerResponseJSON(b []byte) (AnswerResponse, error) {
	var r AnswerResponse
	if err := json.Unmarshal(b, &r); err != nil {
		return AnswerResponse{}, err
	}
	if err := r.Validate(); err != nil {
		return AnswerResponse{}, err
	}
	return r, nil
}
