//go:build linux

package main

import "testing"

// Reading the clock's state needs no privilege; a nil here would leave every Linux Pond unable
// to say whether its clock is the reason remote access is down.
func TestTheKernelSaysWhetherTheClockIsSynchronized(t *testing.T) {
	if clockSynchronized() == nil {
		t.Fatal("adjtimex could not be read without privilege")
	}
}
