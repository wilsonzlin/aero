package signaling

import (
	"errors"
	"fmt"
)

const (
	// version1 is the current signaling schema version used by the relay.
	version1 = 1
)

var (
	errUnsupportedVersion = errors.New("signaling: unsupported version")
	errInvalidSDPType     = errors.New("signaling: invalid session description type")
	errMissingSDP         = errors.New("signaling: missing session description sdp")
)

// sessionDescription is a minimal, JSON-friendly representation of an SDP offer/answer.
//
// We intentionally avoid depending on any WebRTC library type here; this package
// models the protocol surface, not the implementation.
type sessionDescription struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

// offerRequest is sent by the browser/client to the relay.
type offerRequest struct {
	Version int                `json:"version"`
	Offer   sessionDescription `json:"offer"`
}

// answerResponse is returned by the relay in response to an OfferRequest.
type answerResponse struct {
	Version int                `json:"version"`
	Answer  sessionDescription `json:"answer"`
}

func (r offerRequest) Validate() error {
	if r.Version != version1 {
		return fmt.Errorf("%w: %d", errUnsupportedVersion, r.Version)
	}
	if r.Offer.Type != "offer" {
		return fmt.Errorf("%w: %q", errInvalidSDPType, r.Offer.Type)
	}
	if r.Offer.SDP == "" {
		return errMissingSDP
	}
	return nil
}

func (r answerResponse) Validate() error {
	if r.Version != version1 {
		return fmt.Errorf("%w: %d", errUnsupportedVersion, r.Version)
	}
	if r.Answer.Type != "answer" {
		return fmt.Errorf("%w: %q", errInvalidSDPType, r.Answer.Type)
	}
	if r.Answer.SDP == "" {
		return errMissingSDP
	}
	return nil
}
