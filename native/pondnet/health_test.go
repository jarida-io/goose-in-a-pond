package pondnet

import (
	"net"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

// What the backend says about itself reaches a log, so it is held to the same rule as every
// other line: no enrollment capability, one line each, and a bound on how much.
func TestHealthLinesAreRedactedOneLineAndBounded(t *testing.T) {
	long := "fetch control key: " + strings.Repeat("é", maxHealthLineBytes)
	lines := healthLines([]string{"", "   ", "e", "d", "c", "b", "a"})
	if strings.Join(lines, "|") != "a|b|c|d" {
		t.Fatalf("expected the first %d of the sorted, non-empty warnings, got %q", maxHealthLines, lines)
	}

	all := healthLines([]string{long, "You are logged out. https://control.example/register/abcdef0123456789\nnext"})
	for _, line := range all {
		if strings.Contains(line, "/register/") || strings.Contains(line, "abcdef0123456789") {
			t.Fatalf("enrollment capability survived: %q", line)
		}
		if strings.ContainsAny(line, "\n\r") {
			t.Fatalf("a health line spans lines: %q", line)
		}
		if len(line) > maxHealthLineBytes+len("...") {
			t.Fatalf("a health line exceeds its bound: %d bytes", len(line))
		}
		if !strings.HasPrefix(line, "You are logged out.") && !strings.HasSuffix(line, "...") {
			t.Fatalf("a cut line is not marked as cut: %q", line)
		}
		if !utf8.ValidString(line) {
			t.Fatalf("cutting split a character: %q", line)
		}
	}
	if healthLines(nil) != nil {
		t.Fatal("no warnings must report none")
	}
}

// A node whose coordinator cannot be reached never reaches Running (a registered one sits in
// NoState), and before this it said nothing about why: its backend log is discarded. The reason
// now travels in its status.
func TestANodeThatCannotReachItsCoordinatorSaysWhy(t *testing.T) {
	// A port that was just free on loopback: every connection to it is refused.
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	control := "https://" + listener.Addr().String()
	listener.Close()

	node, err := Open(filepath.Join(t.TempDir(), "node"), "pond-test", control)
	if err != nil {
		t.Fatal(err)
	}
	defer node.Close()

	deadline := time.Now().Add(30 * time.Second)
	var status Status
	for time.Now().Before(deadline) {
		if status, err = node.Snapshot(); err != nil {
			t.Fatal(err)
		}
		if len(status.Health) > 0 {
			break
		}
		time.Sleep(200 * time.Millisecond)
	}
	if status.State == "Running" {
		t.Fatal("a node with no coordinator reported Running")
	}
	reason := strings.Join(status.Health, " | ")
	t.Logf("state %s, health %q", status.State, reason)
	if !strings.Contains(reason, listener.Addr().String()) || !strings.Contains(reason, "refused") {
		t.Fatalf("state %s: the reason does not name the refused coordinator: %q", status.State, reason)
	}
}
