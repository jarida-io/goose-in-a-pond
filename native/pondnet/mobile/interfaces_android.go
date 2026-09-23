package mobile

import "tailscale.com/net/netmon"

func init()                              { netmon.RegisterInterfaceGetter(nativeInterfaces) }
func updateDefaultInterface(name string) { netmon.UpdateLastKnownDefaultRouteInterface(name) }
