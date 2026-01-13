package relay

import (
	"net"
	"net/netip"
	"testing"
)

func startIPv6UDPEchoServer(t *testing.T) (*net.UDPConn, netip.AddrPort) {
	t.Helper()

	conn, err := net.ListenUDP("udp6", &net.UDPAddr{IP: net.IPv6loopback, Port: 0})
	if err != nil {
		t.Skipf("ipv6 not supported: %v", err)
	}
	addr := conn.LocalAddr().(*net.UDPAddr).AddrPort()

	go func() {
		buf := make([]byte, 64*1024)
		for {
			n, peer, err := conn.ReadFromUDP(buf)
			if err != nil {
				return
			}
			_, _ = conn.WriteToUDP(buf[:n], peer)
		}
	}()

	return conn, addr
}
