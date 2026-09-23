package mobile

import (
	"encoding/json"
	"errors"
	"net"
	"strings"
	"sync"

	"tailscale.com/net/netmon"
)

// InterfaceProvider supplies OS interface metadata through the permitted native
// API. Android does not allow Go's netlink-based interface enumeration in apps.
// No SSID, hardware address, credentials or peer inventory is collected.
type InterfaceProvider interface {
	GetInterfaces() (string, error)
}

var providerMu sync.RWMutex
var interfaceProvider InterfaceProvider

// SetInterfaceProvider is installed by Android before starting embedded networking.
func SetInterfaceProvider(provider InterfaceProvider) {
	providerMu.Lock()
	interfaceProvider = provider
	providerMu.Unlock()
}
func nativeInterfaces() ([]netmon.Interface, error) {
	providerMu.RLock()
	provider := interfaceProvider
	providerMu.RUnlock()
	if provider == nil {
		return nil, errors.New("native network interface provider is unavailable")
	}
	raw, err := provider.GetInterfaces()
	if err != nil {
		return nil, errors.New("native network interface query failed")
	}
	return parseInterfaces(raw)
}
func parseInterfaces(raw string) ([]netmon.Interface, error) {
	invalid := errors.New("invalid native network interface metadata")
	if len(raw) > 65536 {
		return nil, invalid
	}
	var records []struct {
		Name      string   `json:"name"`
		Index     int      `json:"index"`
		MTU       int      `json:"mtu"`
		Flags     uint     `json:"flags"`
		Addresses []string `json:"addresses"`
	}
	if json.Unmarshal([]byte(raw), &records) != nil || records == nil || len(records) > 128 {
		return nil, invalid
	}
	result := make([]netmon.Interface, 0, len(records))
	seen := map[string]bool{}
	for _, record := range records {
		if record.Name == "" || len(record.Name) > 64 || strings.ContainsAny(record.Name, "\x00\r\n") || record.Index < 1 || record.MTU < 0 || record.Flags > 63 || len(record.Addresses) > 32 || seen[record.Name] {
			return nil, invalid
		}
		seen[record.Name] = true
		item := netmon.Interface{Interface: &net.Interface{Name: record.Name, Index: record.Index, MTU: record.MTU, Flags: net.Flags(record.Flags)}, AltAddrs: make([]net.Addr, 0, len(record.Addresses))}
		for _, address := range record.Addresses {
			ip, network, err := net.ParseCIDR(address)
			if err != nil {
				return nil, invalid
			}
			network.IP = ip
			item.AltAddrs = append(item.AltAddrs, network)
		}
		result = append(result, item)
	}
	return result, nil
}

// NetworkChanged wakes the running userspace node after an OS network transition.
// The native layer registers callbacks only while remote networking is enabled.
func NetworkChanged(defaultInterface string) {
	lock.Lock()
	defer lock.Unlock()
	if node == nil {
		return
	}
	updateDefaultInterface(defaultInterface)
	if monitor, ok := node.Server.Sys().NetMon.GetOK(); ok {
		monitor.InjectEvent()
	}
}
