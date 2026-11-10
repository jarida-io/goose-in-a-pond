# The pond's data at rest: an encrypted volume

Status: built 2026-11-09 (`scripts/lib/data-volume.sh`, `giap.sh data-encrypt`). This is P1's "data
volume" row of [context-space.md section 9.2](context-space.md#92-key-hierarchy), for a pond that
runs in its user's session. The service-account layout (`agamemnon`, data in
`/var/lib/goose-in-a-pond`) is not covered yet.

## The problem

A pond keeps its databases, `secrets.json` and the key beside it, its TLS identity, Matter fabric
credentials, remote-access state, Goose's sessions and saved conversation state as plain files in
its data directory. Storage taken out of the board, or copied, gives all of it up. On 2026-11-09 the
Jetson's SSD was found to work as a pond in another Jetson.

## What changes

The data directory becomes the mount point of a LUKS2 volume kept in one file,
`/var/lib/goose-in-a-pond-data/data.luks` (16 GB by default, ext4 inside). The file is sparse: it
takes only what the volume has written and grows with the pond's data, so the host disk must keep
room for that growth (the doctor's disk-headroom check covers it). The first run on the Jetson
created it fully allocated (`fallocate`). The unused 13 GB came back by copying the closed volume's
file with `cp --sparse=always`, checking the copy with `cmp`, and moving it into place: 16 GB to
3.3 GB, and the TPM opened it as before. `fallocate --dig-holes` did not help: ext4 reports space
reserved but never written as a hole, so it is skipped. Its key is never on the
disk in a usable form. Two keyslots open it:

| Keyslot | Opens it | When |
|---|---|---|
| TPM2 (`systemd-cryptenroll --tpm2-device=auto --tpm2-pcrs=`) | This board's TPM, with nobody present | Every boot |
| The admin card | The household's printed card | Recovery: another board, a cleared TPM, a failed unseal |

The TPM keyslot is bound to no PCRs. Binding it to the firmware's measurements would lock the
pond out after a JetPack update, and on a board without secure boot those measurements prove
little.

Downloaded models and programs are not private and are not encrypted: `hf_cache`, `models`, `bin`,
`lib` and `engine/data/litert-lm` move to `<data dir>-public` and are linked back at their old
paths, so the pond finds them unchanged and the volume stays small (about 3.5 GB in use on the
Jetson on 2026-11-09).

## How a boot opens it

- `goose-in-a-pond-data.service` (root, `Before=user@<uid>.service`) runs
  `/usr/local/libexec/goose-in-a-pond-data open`. It waits up to 60 s for OP-TEE's TPM device, opens
  the volume with `systemd-cryptsetup attach … tpm2-device=auto,headless=true` (never a prompt),
  checks the file system with `fsck -p` and refuses to mount on anything worse than a fixed error,
  then mounts it `nodev,nosuid` on the data directory.
- The pond's user unit gets a drop-in, `ExecStartPre=/usr/bin/mountpoint -q <data dir>`. With the
  volume closed the pond does not start, rather than starting as a new, empty pond.
- Under the mount, the bare directory is empty and immutable (`chattr +i`), so nothing can write
  plain data there while the volume is closed.
- A closed volume is a doctor FAIL with the way in: `journalctl -u goose-in-a-pond-data`, and
  `giap.sh data-encrypt unlock`, which asks for the admin card.

## Moving a pond's data in

`bash scripts/giap.sh data-encrypt` (with `--dry-run` first, which runs nothing):

1. Refuses unless: Linux, a user-session pond, not already encrypted, every tool present
   (`cryptsetup`, `systemd-cryptenroll`, `losetup`, `mkfs.ext4`, `rsync`, `truncate`, `chattr`,
   `findmnt`, `mountpoint`, `fstrim`), a TPM, a sealed admin card to enroll, the private data under
   80% of the volume, and room for a copy of it and 2 GB.
2. Stops the pond and moves the downloads out.
3. Creates the volume under a one-time random key in `/run`, adds the admin card and the TPM, then
   removes the one-time key. The card reaches `cryptsetup` and `systemd-cryptenroll` on stdin, never
   in an argument list.
4. Opens the volume with the TPM, as a boot does, before anything is copied: a TPM that cannot open
   it stops the move with nothing changed.
5. Copies the private data in with `rsync -aHAX`, and checks the file count and the checksums of the
   databases and `secrets.json`. Any difference stops before the switch.
6. Keeps the plain directory as `<data dir>.plain-<time>`, mounts the volume in its place, installs
   the unit, helper and drop-in, and starts the pond.
7. `data-encrypt finish` refuses until the board has booted since the move, so a boot has shown the
   TPM opens it unattended; then it deletes the plain copy and runs
   `fstrim`. Deleted files can stay readable in an SSD's free blocks; a discard is the best that can
   be done short of reformatting the storage.

`data-encrypt undo` reverses it: the data back to plain files, the downloads back in place, the
volume, unit, helper and drop-in removed.

## The admin card

`giap.sh admin-card rotate` reads the old card's sealed copy before replacing it and moves the
volume's recovery keyslot to the new card (`cryptsetup luksChangeKey`). Without the old card's
sealed copy it says the volume still opens with the old card.

## What it stops, and what it does not

| Threat | Stopped? |
|---|---|
| A copy of the storage read on another computer | Yes. Without the board's TPM or the card, the volume is ciphertext |
| The storage moved into another Jetson | Only once this board's security fuses are burned. Until then the doctor says so, see below |
| The whole board taken, running | No. An open volume is open; this protects data at rest |
| The whole board taken, on an unfused board | No. Without secure boot, a modified OS can run as root and unseal (context-space decision 2) |
| A process on the running pond | No; the authorization model and the egress gate cover that |

**Unfused boards.** NVIDIA documents the fTPM's seed as derived from a per-device secret burned into
the KDK0 fuse, with a per-device EKB, and says nothing of what an unfused board uses
([Firmware TPM, R36.4](https://docs.nvidia.com/jetson/archives/r36.4/DeveloperGuide/SD/Security/FirmwareTPM.html)).
Development images can inject a fixed EPS. So on an unfused board the TPM's secret may be the same
on every unfused Jetson, and a volume sealed to it may open in another one. The doctor says this
under every encrypted pond. Burning the fuses (OEM_K1, KDK0, secure boot) is irreversible, needs
NVIDIA's FSKP tooling and a reflash, and is planned as its own step after a rehearsal on a spare
board (context-space decision 2, P8).

## Checking it

- `bash scripts/lib/data-volume.test.sh`: refusals, the dry run's order (TPM open before copy, the
  one-time key removed after both keys, `chattr` before the mount), that the card is never in an
  argument, the boot helper and unit, the drop-in, card rotation, `finish` and the private-size count.
- `bash scripts/rehearse-data-volume.sh` on a Jetson: the whole flow on scratch data with the real TPM
  and LUKS, named `giap-dv-rehearsal`, never touching the pond. It encrypts, closes and reopens
  through the unit as a boot does, opens with a test card, rotates it, decrypts and removes
  everything.
- `bash scripts/giap.sh doctor` reports the state on every Linux pond.

**Rehearsed on the Jetson 2026-11-09** (Orin Nano, L4T R36.4.7, systemd 249, cryptsetup 2.4.3): all
26 checks passed. The TPM opened the volume with nobody present ("Automatically discovered security
TPM2 token unlocks volume"); its token reads `"tpm2-pcrs":[]`; two keyslots remained (card and
TPM); closed, the directory was empty and refused a write; the unit reopened it with the TPM; the
rotated card opened it and the old one did not; undo restored the plain files; nothing was left
behind. Adding the card's keyslot took the longest: LUKS2's Argon2 asks for about 650 MB, and with
the pond's model loaded the board had about 545 MB free. `data-encrypt` stops the pond first, so a
real run has that memory.

**Run on the Jetson's own data 2026-11-09.** 3,026 MB of private data, 28,725 files, moved into the
volume with the databases and secrets matching; the pond came back on it with its 1,913 messages
and its settings. After a reboot at 12:57:42 the unit opened the volume with the TPM alone at
12:57:58 (`pond-data: clean`), and the pond started a second later with no restart. `finish` then
removed the plain copy and trimmed 16.2 GiB. (`finish` ran before that reboot on this first run;
it now refuses until the board has booted since the move.)
