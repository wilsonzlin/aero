// Package relay contains UDP relay primitives that move datagrams between
// WebRTC DataChannels and UDP sockets.
//
// The relay MUST enforce the destination policy from internal/policy to avoid
// becoming an open UDP proxy.
package relay
