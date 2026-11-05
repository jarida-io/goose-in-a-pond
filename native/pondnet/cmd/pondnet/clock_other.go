//go:build !linux

package main

// clockSynchronized cannot be read here without a privileged or platform-specific call, so it is
// left out of the report rather than guessed.
func clockSynchronized() *bool { return nil }
