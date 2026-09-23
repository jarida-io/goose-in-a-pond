package pondnet

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"os"
	"path/filepath"
	"testing"
)

func TestAuthorityPersistenceAndSignature(t *testing.T) {
	path := filepath.Join(t.TempDir(), "authority")
	first, err := LoadAuthority(path)
	if err != nil {
		t.Fatal(err)
	}
	restored, err := LoadAuthority(path)
	if err != nil {
		t.Fatal(err)
	}
	if first.Household != restored.Household || first.PublicKey != restored.PublicKey {
		t.Fatal("authority changed after restart")
	}
	envelope, err := enrollment.Sign(enrollment.Approval{Household: first.Household, Device: "paired-device-0001", Action: "enroll"}, restored.key)
	if err != nil {
		t.Fatal(err)
	}
	payload, _ := base64.StdEncoding.DecodeString(envelope.Payload)
	signature, _ := base64.StdEncoding.DecodeString(envelope.Signature)
	public, _ := base64.StdEncoding.DecodeString(first.PublicKey)
	if !ed25519.Verify(public, append([]byte("goose-enrollment-v1\x00"), payload...), signature) {
		t.Fatal("restored authority cannot sign")
	}
	if ed25519.Verify(public, payload, signature) {
		t.Fatal("signature lacks domain separation")
	}
	exposed, _ := json.Marshal(first)
	var fields map[string]any
	json.Unmarshal(exposed, &fields)
	if len(fields) != 2 || fields["publicKey"] == nil || fields["household"] == nil {
		t.Fatal("private authority leaked")
	}
	info, _ := os.Stat(filepath.Join(path, "identity.json"))
	if info.Mode().Perm()&0077 != 0 {
		t.Fatal("identity is not private")
	}
}
func TestAuthorityRefusesDamagedState(t *testing.T) {
	for _, damage := range []string{"missing", "truncated", "permissive", "symlink", "trailing", "seed"} {
		t.Run(damage, func(t *testing.T) {
			dir := filepath.Join(t.TempDir(), "authority")
			if _, err := LoadAuthority(dir); err != nil {
				t.Fatal(err)
			}
			p := filepath.Join(dir, "identity.json")
			switch damage {
			case "missing":
				os.Remove(p)
			case "truncated":
				os.WriteFile(p, []byte("{"), 0600)
			case "permissive":
				os.Chmod(p, 0644)
			case "symlink":
				os.Rename(p, p+".backup")
				os.Symlink(p+".backup", p)
			case "trailing":
				f, _ := os.OpenFile(p, os.O_APPEND|os.O_WRONLY, 0600)
				f.WriteString("{}")
				f.Close()
			case "seed":
				os.WriteFile(p, []byte(`{"seed":"AA=="}`), 0600)
			}
			if _, err := LoadAuthority(dir); err == nil {
				t.Fatal("damaged identity accepted")
			}
		})
	}
}
