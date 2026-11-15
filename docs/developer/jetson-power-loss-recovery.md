# Bringing the pond back after a power cut

**In the normal case you do nothing.** The service is enabled (and, for a user unit, lingering is
on), so the pond starts at boot without anyone logging in. This page is for when it did not come
back, or came back wrong, and for telling which in under a minute.

Commands run from the Mac unless marked `[on the Jetson]`. They use the ssh alias `nano` and the
**user** unit that `scripts/jetson.sh deploy` manages (`systemctl --user`). A board set up by
`scripts/install.sh` runs a **system** unit of the same name instead: drop `--user` and add `sudo`.
`bash scripts/giap.sh status` on the board says which scope it has.

## 1. Is it already fine?

```bash
ssh nano 'systemctl --user is-active goose-in-a-pond.service; curl -s -o /dev/null -w "%{http_code}\n" --max-time 5 http://127.0.0.1:8080/api/v1/health'
```

`active` and `200` means the pond is up. If `ssh` itself fails, go to [section 5](#5-it-is-not-on-the-network).

### What "up" looks like

```bash
ssh nano 'ss -lntp | grep pond-server'
```

| Listener | Reach | What it is |
|---|---|---|
| `127.0.0.1:8080` | loopback only | Plain HTTP. Reachable from the board itself or through an SSH tunnel |
| `0.0.0.0:4443` (normally) | the whole LAN | HTTPS with the pinned certificate; what the phone talks to |

If 8080 ever shows as `0.0.0.0:8080`, fix that first: it is meant to be unreachable from the network.

## 2. It is not running

```bash
ssh nano 'systemctl --user start goose-in-a-pond.service'
```

Startup applies migrations, sizes the context, loads the embedder and warms the prompt prefix
**before** it binds a port, which is tens of seconds on this board. Poll rather than guess; `000`
means nothing is listening yet, which is normal while the model loads:

```bash
ssh nano 'for i in $(seq 1 30); do c=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 http://127.0.0.1:8080/api/v1/health); [ "$c" = 200 ] && { echo "up after ${i}0s"; break; }; sleep 10; done'
```

### If it starts and then dies

```bash
ssh nano 'systemctl --user status goose-in-a-pond.service --no-pager | head -20'
ssh nano 'journalctl --user -u goose-in-a-pond.service -b --no-pager | tail -60'
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh logs'
```

`Restart=on-failure` makes a crash loop look like a service that is forever starting. The full log
is the daily file `~/.local/share/goose-in-a-pond/logs/pond.log.YYYY-MM-DD` (UTC); journald holds
only stdout and stderr, which is where a crash before logging started shows up.

## 3. It is running but the model will not load

The failure that actually follows a power cut, and it does not look like a memory problem. After a
boot the page cache fills with whatever was read on the way up. NvMap, the allocator the GPU uses,
**cannot reclaim page cache**, so a small allocation (136, 143 and 258 MiB have been seen) fails
while `free -h` reports gigabytes available. It surfaces as an allocation failure, not as out of
memory.

```bash
ssh nano 'free -h'
ssh nano 'systemctl --user stop goose-in-a-pond.service && sudo sh -c "sync; echo 3 > /proc/sys/vm/drop_caches" && systemctl --user start goose-in-a-pond.service'
```

Drop the caches **while the service is stopped**: under a running pond it frees memory the pond is
about to want back.

## 4. It is running but answers slowly, or without tools

Decode here is memory-bandwidth-bound: `tok/s = 102 / model_GB` with the model on the GPU, more with
speculative decoding on a Gemma 4 E2B or E4B model. Much worse than that, in order of likelihood:

1. **The build lost CUDA.** It looks completely normal and is many times slower.
   ```bash
   ssh nano 'grep -E "Applied Jetson Orin Nano CUDA|CPU-ONLY" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F) | tail -1'
   ```
   `CPU-ONLY (built without the cuda feature)` means rebuild (`bash scripts/jetson.sh deploy`).
2. **A debug binary is being served, or none at all.** The unit's `ExecStart` points at the installed
   `~/.local/lib/goose-in-a-pond/bin/pond-server` (an older unit at `target/release/pond-server`).
   `status=203/EXEC` in `systemctl --user status goose-in-a-pond` means that file is gone: a unit
   still running from `target/` loses it to `cargo clean`. Rebuild with `bash scripts/giap.sh build`,
   which installs it and repoints the unit; `giap.sh doctor` names which binary the unit runs.
3. **Something else holds the GPU or the cores**, a second `pond-server` above all:
   `ssh nano 'ps aux --sort=-%cpu | head -6'`.

## 5. It is not on the network

Every deploy records the address the board answered at in `~/.giap/jetson/nano.address` on the Mac.
If that address answers `ping` but `ssh nano` does not, the name (mDNS) is the problem, not the
pond: `ssh -o HostName=<address> nano` works meanwhile, and the next deploy falls back to the
recorded address by itself.

Console access needs a monitor and keyboard on the board. `[on the Jetson]` Wi-Fi is managed by
NetworkManager (the interface is `wlP1p1s0`):

```bash
nmcli -t -f DEVICE,STATE,CONNECTION device
nmcli connection up <connection>
```

### DNS

A home router that drops AAAA queries does not present as a DNS fault. It presents as everything
being slow: 15-second name lookups, the weather tool timing out at 10 seconds, `npm` failing with
`ENOTFOUND`. Point the Wi-Fi link at public resolvers and ask the link by name (`resolvectl status`
on its own prints a global fallback line and other links above it):

```bash
ssh nano 'resolvectl dns wlP1p1s0'
ssh nano 'sudo nmcli connection modify <connection> ipv4.ignore-auto-dns yes ipv4.dns "1.1.1.1 8.8.8.8" && sudo nmcli connection up <connection>'
```

The same router affects the Mac and the phone independently; each needs its own fix (on the phone,
Private DNS set to `one.one.one.one`).

## 6. Reaching it from the Mac

```bash
ssh -N -L 8080:127.0.0.1:8080 nano     # then http://127.0.0.1:8080
```

The tunnel keeps the browser's origin on loopback, which the auth path expects; the HTTPS port
presents the pinned certificate, which a browser has no reason to trust. The phone needs none of
this: it talks to the HTTPS port and pins the certificate itself.

**Do not set `POND_DEV_ALLOW_LOOPBACK` to work around an auth problem.** With it on, anything that
reaches loopback drives the whole API with no token. If it was set for a debugging session, unset it
and restart.

## 7. The sidecars

```bash
ssh nano 'ss -lntp | grep -E "node|pondnet"'
```

- **`node` on `127.0.0.1:5580`**: the matter.js controller. Without it Matter devices stop
  responding while everything else works.
- **`pondnet`**: the remote-access helper, when it was built (it needs Go at build time).

The server starts both. If one is missing, restart the service; they are not started by hand.

## 8. After an unclean shutdown, check the databases

A power cut during a write leaves WAL files behind; SQLite recovers from them on open, so their
presence is normal. What matters is that the pond opened them without complaint:

```bash
ssh nano 'ls -la ~/.local/share/goose-in-a-pond/*.db*'
ssh nano 'grep -iE "migrat|corrupt|sqlite" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F) | head'
```

`pond_system.db` is authoritative: devices, schedules, memory, settings, sessions. `pond_logs.db` is
disposable. A genuinely damaged system database is a restore, not a repair, and it is the one thing
on the board worth backing up.

## 9. Remote access after a power cut

A phone on Wi-Fi at home reaches the pond over the LAN, so a pond that works at home but not
from mobile data has lost only remote access: its tailnet node is short of `Running`. Two
things after a power cut keep it there, and the log now tells them apart.

**No internet.** The router can come back with its Wi-Fi up and its internet link not, for many
minutes. The board joins the Wi-Fi at once, so everything on the LAN works, but the node cannot
reach its coordinator. On 2026-10-31 this lasted about 45 minutes: NetworkManager stayed at
`CONNECTED_SITE` and every DNS query to the router's resolvers timed out, until the access point
dropped for 50 seconds and came back with the internet. The clock synchronised seconds later and
the node was `Running` 24 seconds after that. Nothing on the pond needs fixing for this.

```bash
ssh nano 'nmcli -t -f STATE general'     # "connected (site only)" is the LAN without the internet
```

**The clock.** The board has no working real-time clock. At boot `systemd-timesyncd` restores
the last time it recorded, behind by however long the board was off, and the clock stays there
until a time server answers. A clock behind a certificate's start date fails TLS to the
coordinator; a few hours behind is harmless today (its certificates start weeks earlier), a
board that was off longer than the time since the last renewal is not. The stock configuration
asks only the public NTP pool, through NVIDIA's `nv-fallback-ntp.conf`:

```bash
ssh nano 'timedatectl show -p NTPSynchronized --value; timedatectl show-timesync -p SystemNTPServers -p FallbackNTPServers'
ssh -t nano 'cd goose-in-a-pond && bash scripts/giap.sh --dry-run time-sync'   # what it would write
ssh -t nano 'cd goose-in-a-pond && bash scripts/giap.sh time-sync'             # write it (sudo)
```

`giap.sh time-sync` writes `/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf` with
`NTP=time.cloudflare.com time.google.com 0.pool.ntp.org 1.pool.ntp.org` and restarts
`systemd-timesyncd`; `time-sync remove` undoes it. The pool goes in `NTP=` because timesyncd
ignores `FallbackNTP=` once any `NTP=` server is set. `giap.sh doctor` warns when the clock is not
synchronised or timesyncd has only the pool to ask, and `giap.sh update` offers the change once.

**What the log says.** Two minutes short of `Running` gives one
`kind=remote_access_not_running` WARN naming the node's own reason and `clock_synchronized`;
the recovery gives `kind=remote_access_running_again` with how long it was down. A DNS or
connect timeout in `reason` is the first case, a certificate date error with
`clock_synchronized=no` the second. The node retries on its own within 45 seconds of the cause
clearing; it needs no restart. See [remote access](../remote-access.md#when-the-node-is-not-running).

```bash
ssh nano 'grep -E "remote_access_(not_running|running_again)|network_helper_output" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F)'
```

## The short version

```bash
# Is it up?
ssh nano 'systemctl --user is-active goose-in-a-pond.service; curl -s -o /dev/null -w "%{http_code}\n" --max-time 5 http://127.0.0.1:8080/api/v1/health'

# Start it
ssh nano 'systemctl --user start goose-in-a-pond.service'

# Start it after a power cut, when the model will not load
ssh nano 'systemctl --user stop goose-in-a-pond.service && sudo sh -c "sync; echo 3 > /proc/sys/vm/drop_caches" && systemctl --user start goose-in-a-pond.service'

# Why did it fail
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh logs'

# Reach the dashboard
ssh -N -L 8080:127.0.0.1:8080 nano    # then http://127.0.0.1:8080
```
