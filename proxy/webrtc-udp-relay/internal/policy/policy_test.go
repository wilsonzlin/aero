package policy

import (
	"net"
	"testing"
)

func TestCIDRPrecedence_DenyOverridesAllow(t *testing.T) {
	p := &DestinationPolicy{
		DefaultAllow:         false,
		AllowPrivateNetworks: true,
		AllowCIDRs:           []*net.IPNet{mustCIDR("1.1.1.0/24")},
		DenyCIDRs:            []*net.IPNet{mustCIDR("1.1.1.1/32")},
	}
	if err := p.AllowUDP(net.ParseIP("1.1.1.1"), 53); err == nil {
		t.Fatalf("expected deny to override allow")
	}
}

func TestCIDRAllowOnlyMode(t *testing.T) {
	p := &DestinationPolicy{
		DefaultAllow:         false,
		AllowPrivateNetworks: true,
		AllowCIDRs:           []*net.IPNet{mustCIDR("8.8.8.0/24")},
	}

	if err := p.AllowUDP(net.ParseIP("8.8.8.8"), 53); err != nil {
		t.Fatalf("expected allow, got %v", err)
	}
	if err := p.AllowUDP(net.ParseIP("1.1.1.1"), 53); err == nil {
		t.Fatalf("expected deny when not in allowlist")
	}
}

func TestPrivateNetworksToggle(t *testing.T) {
	ip := net.ParseIP("10.0.0.1")

	denyPrivate := &DestinationPolicy{
		DefaultAllow:         true,
		AllowPrivateNetworks: false,
	}
	if err := denyPrivate.AllowUDP(ip, 53); err == nil {
		t.Fatalf("expected private IP to be denied when AllowPrivateNetworks=false")
	}

	allowPrivate := &DestinationPolicy{
		DefaultAllow:         true,
		AllowPrivateNetworks: true,
	}
	if err := allowPrivate.AllowUDP(ip, 53); err != nil {
		t.Fatalf("expected private IP to be allowed when AllowPrivateNetworks=true, got %v", err)
	}
}

func TestPortAllowDeny(t *testing.T) {
	p := &DestinationPolicy{
		DefaultAllow:         true,
		AllowPrivateNetworks: true,
		AllowPorts:           []PortRange{{Start: 53, End: 53}},
		DenyPorts:            []PortRange{{Start: 53, End: 53}},
	}
	if err := p.AllowUDP(net.ParseIP("8.8.8.8"), 53); err == nil {
		t.Fatalf("expected deny port to override allow")
	}

	p = &DestinationPolicy{
		DefaultAllow:         true,
		AllowPrivateNetworks: true,
		AllowPorts:           []PortRange{{Start: 53, End: 53}},
	}
	if err := p.AllowUDP(net.ParseIP("8.8.8.8"), 53); err != nil {
		t.Fatalf("expected allowed port, got %v", err)
	}
	if err := p.AllowUDP(net.ParseIP("8.8.8.8"), 54); err == nil {
		t.Fatalf("expected non-allowlisted port to be denied")
	}
}

