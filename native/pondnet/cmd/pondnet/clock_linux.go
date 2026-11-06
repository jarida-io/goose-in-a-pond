//go:build linux

package main

import "golang.org/x/sys/unix"

// clockSynchronized is the kernel's answer to whether the system clock is disciplined by a time
// source, the same reading `timedatectl show -p NTPSynchronized` reports. Nil when it cannot be
// read. A board with no working real-time clock boots with whatever time was last recorded, and
// is unsynchronised until a time server answers.
func clockSynchronized() *bool {
	var timex unix.Timex
	state, err := unix.Adjtimex(&timex)
	if err != nil {
		return nil
	}
	synchronized := state != unix.TIME_ERROR && timex.Status&unix.STA_UNSYNC == 0
	return &synchronized
}
