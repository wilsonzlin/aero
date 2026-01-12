package policy

import (
	"errors"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
)

// DestinationPolicy controls which UDP destinations the relay is allowed to send to.
//
// Policy evaluation order:
//  1. Port denylist
//  2. Port allowlist (if configured)
//  3. Default private/special-range denies (when AllowPrivateNetworks=false)
//  4. CIDR denylist
//  5. CIDR allowlist (if configured), otherwise DefaultAllow
//
// Deny rules always override allow rules.
type DestinationPolicy struct {
	// DefaultAllow controls the implicit decision when no allowlist is configured.
	//
	// - production preset: false (deny by default)
	// - dev preset: true (allow by default)
	DefaultAllow bool

	// AllowPrivateNetworks toggles the built-in denylist of common private/special IPv4 ranges.
	//
	// When false, the policy denies at minimum the ranges documented in README.md.
	AllowPrivateNetworks bool

	AllowCIDRs []*net.IPNet
	DenyCIDRs  []*net.IPNet

	AllowPorts []PortRange
	DenyPorts  []PortRange
}

type PortRange struct {
	Start uint16
	End   uint16
}

func NewProductionDestinationPolicy() *DestinationPolicy {
	return &DestinationPolicy{
		DefaultAllow:         false,
		AllowPrivateNetworks: false,
	}
}

func NewDevDestinationPolicy() *DestinationPolicy {
	return &DestinationPolicy{
		DefaultAllow:         true,
		AllowPrivateNetworks: true,
	}
}

// NewDestinationPolicyFromEnv builds a DestinationPolicy from environment variables.
//
// Supported env vars are documented in proxy/webrtc-udp-relay/README.md.
func NewDestinationPolicyFromEnv() (*DestinationPolicy, error) {
	preset := strings.ToLower(strings.TrimSpace(firstEnv("DESTINATION_POLICY_PRESET", "UDP_DESTINATION_POLICY_PRESET", "POLICY_PRESET")))
	var p *DestinationPolicy
	switch preset {
	case "", "prod", "production":
		p = NewProductionDestinationPolicy()
	case "dev", "development":
		p = NewDevDestinationPolicy()
	default:
		return nil, fmt.Errorf("destination policy: unknown DESTINATION_POLICY_PRESET %q", preset)
	}

	if v, ok := os.LookupEnv("ALLOW_PRIVATE_NETWORKS"); ok {
		b, err := strconv.ParseBool(strings.TrimSpace(v))
		if err != nil {
			return nil, fmt.Errorf("destination policy: invalid ALLOW_PRIVATE_NETWORKS %q", v)
		}
		p.AllowPrivateNetworks = b
	}

	if v := strings.TrimSpace(os.Getenv("ALLOW_UDP_CIDRS")); v != "" {
		nets, err := parseCIDRList(v)
		if err != nil {
			return nil, fmt.Errorf("destination policy: invalid ALLOW_UDP_CIDRS: %w", err)
		}
		p.AllowCIDRs = nets
	}

	if v := strings.TrimSpace(os.Getenv("DENY_UDP_CIDRS")); v != "" {
		nets, err := parseCIDRList(v)
		if err != nil {
			return nil, fmt.Errorf("destination policy: invalid DENY_UDP_CIDRS: %w", err)
		}
		p.DenyCIDRs = nets
	}

	if v := strings.TrimSpace(os.Getenv("ALLOW_UDP_PORTS")); v != "" {
		ranges, err := parsePortRangeList(v)
		if err != nil {
			return nil, fmt.Errorf("destination policy: invalid ALLOW_UDP_PORTS: %w", err)
		}
		p.AllowPorts = ranges
	}

	if v := strings.TrimSpace(os.Getenv("DENY_UDP_PORTS")); v != "" {
		ranges, err := parsePortRangeList(v)
		if err != nil {
			return nil, fmt.Errorf("destination policy: invalid DENY_UDP_PORTS: %w", err)
		}
		p.DenyPorts = ranges
	}

	return p, nil
}

func (p *DestinationPolicy) AllowUDP(remoteIP net.IP, remotePort uint16) error {
	if p == nil {
		return errors.New("destination policy: nil")
	}
	if remoteIP == nil {
		return errors.New("destination policy: remote IP is nil")
	}
	if remotePort == 0 {
		return errors.New("destination policy: remote port 0 is invalid")
	}

	if portInRanges(remotePort, p.DenyPorts) {
		return fmt.Errorf("destination policy: UDP port %d denied", remotePort)
	}
	if len(p.AllowPorts) > 0 && !portInRanges(remotePort, p.AllowPorts) {
		return fmt.Errorf("destination policy: UDP port %d not in allowlist", remotePort)
	}

	// Normalize IP for matching.
	ip := remoteIP
	var ipKind string
	if ip4 := remoteIP.To4(); ip4 != nil {
		ip = ip4
		ipKind = "ipv4"
	} else if ip16 := remoteIP.To16(); ip16 != nil {
		ip = ip16
		ipKind = "ipv6"
	} else {
		return fmt.Errorf("destination policy: invalid remote IP %q", remoteIP.String())
	}

	if !p.AllowPrivateNetworks {
		switch ipKind {
		case "ipv4":
			if ipInNets(ip, defaultDeniedIPv4CIDRs) {
				return fmt.Errorf("destination policy: UDP destination %s denied (private/special range)", remoteIP.String())
			}
		case "ipv6":
			// Not required by the prompt, but we defensively block common private/special IPv6 ranges
			// when private networks are disabled.
			if ipInNets(ip, defaultDeniedIPv6CIDRs) {
				return fmt.Errorf("destination policy: UDP destination %s denied (private/special range)", remoteIP.String())
			}
		}
	}

	if ipInNets(ip, p.DenyCIDRs) {
		return fmt.Errorf("destination policy: UDP destination %s denied by CIDR rule", remoteIP.String())
	}

	if len(p.AllowCIDRs) > 0 {
		if ipInNets(ip, p.AllowCIDRs) {
			return nil
		}
		return fmt.Errorf("destination policy: UDP destination %s not in allowlist", remoteIP.String())
	}

	if p.DefaultAllow {
		return nil
	}
	return fmt.Errorf("destination policy: UDP destination %s denied by default", remoteIP.String())
}

func firstEnv(keys ...string) string {
	for _, k := range keys {
		if v, ok := os.LookupEnv(k); ok {
			return v
		}
	}
	return ""
}

func parseCIDRList(v string) ([]*net.IPNet, error) {
	var out []*net.IPNet
	for _, raw := range strings.Split(v, ",") {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		_, n, err := net.ParseCIDR(raw)
		if err != nil {
			return nil, fmt.Errorf("parse CIDR %q: %w", raw, err)
		}
		out = append(out, n)
	}
	return out, nil
}

func parsePortRangeList(v string) ([]PortRange, error) {
	var out []PortRange
	for _, raw := range strings.Split(v, ",") {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		startStr, endStr, hasRange := strings.Cut(raw, "-")
		start, err := parsePort(strings.TrimSpace(startStr))
		if err != nil {
			return nil, err
		}
		end := start
		if hasRange {
			end, err = parsePort(strings.TrimSpace(endStr))
			if err != nil {
				return nil, err
			}
			if start > end {
				return nil, fmt.Errorf("invalid port range %q: start > end", raw)
			}
		}
		out = append(out, PortRange{Start: start, End: end})
	}
	return out, nil
}

func parsePort(v string) (uint16, error) {
	if v == "" {
		return 0, errors.New("empty port")
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return 0, fmt.Errorf("invalid port %q", v)
	}
	if n < 1 || n > 65535 {
		return 0, fmt.Errorf("port %d out of range", n)
	}
	return uint16(n), nil
}

func portInRanges(port uint16, ranges []PortRange) bool {
	for _, r := range ranges {
		if port >= r.Start && port <= r.End {
			return true
		}
	}
	return false
}

func ipInNets(ip net.IP, nets []*net.IPNet) bool {
	ip4 := ip.To4()
	var ip16 net.IP
	for _, n := range nets {
		if n == nil {
			continue
		}
		if n.IP.To4() != nil {
			if ip4 == nil {
				continue
			}
			if n.Contains(ip4) {
				return true
			}
			continue
		}
		if ip16 == nil {
			ip16 = ip.To16()
		}
		if ip16 == nil {
			continue
		}
		if n.Contains(ip16) {
			return true
		}
	}
	return false
}

func mustCIDR(s string) *net.IPNet {
	_, n, err := net.ParseCIDR(s)
	if err != nil {
		panic(err)
	}
	return n
}

var defaultDeniedIPv4CIDRs = []*net.IPNet{
	// loopback
	mustCIDR("127.0.0.0/8"),
	// link-local
	mustCIDR("169.254.0.0/16"),
	// RFC1918 private
	mustCIDR("10.0.0.0/8"),
	mustCIDR("172.16.0.0/12"),
	mustCIDR("192.168.0.0/16"),
	// CGNAT
	mustCIDR("100.64.0.0/10"),
	// multicast
	mustCIDR("224.0.0.0/4"),
	// reserved
	mustCIDR("0.0.0.0/8"),
	mustCIDR("240.0.0.0/4"),
	// broadcast
	mustCIDR("255.255.255.255/32"),
}

var defaultDeniedIPv6CIDRs = []*net.IPNet{
	// loopback
	mustCIDR("::1/128"),
	// link-local
	mustCIDR("fe80::/10"),
	// unique local addresses (RFC4193)
	mustCIDR("fc00::/7"),
	// multicast
	mustCIDR("ff00::/8"),
	// unspecified
	mustCIDR("::/128"),
}
