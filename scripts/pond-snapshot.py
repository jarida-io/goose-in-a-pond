#!/usr/bin/env python3
"""Stream a tar of the Pond's irreplaceable state to stdout.

Everything here is either unrecoverable (the household authority, the HTTPS identity,
the WireGuard node state, the master key) or cheap to keep. Models, hf_cache, binaries
and logs are excluded: 8.2G, all refetchable.

Databases are copied through SQLite's online backup API, so the Pond keeps serving and
the copy is consistent rather than a torn read of a WAL database.
"""
import os, shutil, sqlite3, sys, tarfile, tempfile

DATA = os.path.expanduser("~/.local/share/goose-in-a-pond")
DBS = ["pond_system.db", "pond_vectors.db"]
TREES = ["embedded-network", "tls", "secrets"]
FILES = ["secrets.json", "schedules.json", "schedule_runs.json"]
# Locks are per-process artefacts; the tailscaled logs are noise.
SKIP = (".lock", "tailscaled.log.conf", "tailscaled.log1.txt", "tailscaled.log2.txt")

def consistent_copy(src, dst):
    s = sqlite3.connect("file:%s?mode=ro" % src, uri=True)
    d = sqlite3.connect(dst)
    with d:
        s.backup(d)
    d.close(); s.close()

staged = tempfile.mkdtemp(prefix="pond-snapshot-")
try:
    for name in DBS:
        p = os.path.join(DATA, name)
        if os.path.exists(p):
            consistent_copy(p, os.path.join(staged, name))
    for tree in TREES:
        src = os.path.join(DATA, tree)
        if os.path.isdir(src):
            shutil.copytree(src, os.path.join(staged, tree),
                            ignore=lambda d, names: [n for n in names if n.endswith(SKIP)])
    for name in FILES:
        p = os.path.join(DATA, name)
        if os.path.exists(p):
            shutil.copy2(p, os.path.join(staged, name))

    out = tarfile.open(mode="w|", fileobj=sys.stdout.buffer)
    for entry in sorted(os.listdir(staged)):
        out.add(os.path.join(staged, entry), arcname=entry)
    out.close()
finally:
    shutil.rmtree(staged, ignore_errors=True)
