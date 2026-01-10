// Package policy defines the destination policy used to constrain outbound UDP
// egress from the relay.
//
// A UDP relay is network egress and can easily become an open proxy / SSRF
// primitive. The DestinationPolicy type is designed to be evaluated before
// opening a UDP binding and before sending each datagram.
package policy
