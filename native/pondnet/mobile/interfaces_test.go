package mobile

import (
	"strings"
	"testing"
)

func TestNativeInterfaceMetadata(t *testing.T) {
	result, err := parseInterfaces(`[{"name":"wlan0","index":2,"mtu":1500,"flags":19,"addresses":["192.168.1.7/24","fe80::1234/64"]},{"name":"empty0","index":3,"mtu":1500,"flags":0,"addresses":[]}]`)
	if err != nil || len(result) != 2 {
		t.Fatal("valid interfaces rejected", err)
	}
	addresses, err := result[0].Addrs()
	if err != nil || addresses[0].String() != "192.168.1.7/24" || addresses[1].String() != "fe80::1234/64" {
		t.Fatal("host address was masked", addresses, err)
	}
	if result[1].AltAddrs == nil {
		t.Fatal("empty addresses would fall back to forbidden netlink query")
	}
	for _, raw := range []string{"null", "{}", strings.Repeat(" ", 65537), `[{"name":"wlan0","index":0}]`, `[{"name":"wlan0","index":2,"addresses":["invalid"]}]`, `[{"name":"wlan0","index":2},{"name":"wlan0","index":3}]`} {
		if _, err := parseInterfaces(raw); err == nil {
			t.Fatalf("invalid metadata accepted: %.80s", raw)
		}
	}
}
