# Jetson device tuning — measured

> Status (2026-10-23): a record of the 2026-09-07 preparation. The budget constants it cites now live in
> `crates/pond-core/src/models/domain/device_budget.rs` (`LLM_BUDGET_MB` is still 5,820), and the power mode
> settled on 25 W (`nvpmodel -m 1`); see [jetson-dev-routine.md](jetson-dev-routine.md).

Orin Nano 8 GB Super, JetPack 6.2.1 / L4T r36.4.7, kernel 5.15.148-tegra, CUDA 12.6.
Prep for the inference-engine bake-off, 2026-09-07. Raw baseline:
[`jetson-baseline-2026-09-07/`](jetson-baseline-2026-09-07/).

Every number here was read off the device. Where a figure conflates two effects,
it is split; where a figure is a fallback rather than a measurement, it says so.

## Headline

| | before | after |
|---|---:|---:|
| Disk free | 1.4 GB (99 % full) | **40 GB** (64 %) |
| RAM available (idle) | 2,219 MiB | **6,804 MiB** |
| nvmap / GPU memory held at idle | 2,594 MiB | **0 MiB** |
| Swap total | 12,001 MB | **5,857 MB** |

## 1. Disk — 34 GB reclaimed

Three deletions, each gated on a pre-check that nothing referenced the target.

| Removed | Size | Pre-check |
|---|---:|---|
| `dustynv/mlc:0.20.0-r36.4.0` image | 24.4 GB | 0 containers; MLC was measured and rejected for this stack |
| `~/bench-models/gemma-4-E4B-it-Q4_K_M.gguf` | 5.0 GB | no symlink, registry or unit referenced it |
| orphan blob `2c9f928b…` (E4B-Q4_K_M) | 4.7 GB | exactly one pointer (its own snapshot symlink), 0 registry hits, not open |

The live `6a6667bc…` (E4B-IQ4_XS) blob was untouched and all four
`models/gguf` entries still resolve. The orphan was also the worst-ranked of the
three E4B options by the context derivation (ctx 8192, 26 MB spare), superseded
by the QAT weights.

## 2. Swap — 12,001 MB → 5,857 MB

`/swapfile` 8 GB → 2 GB; the six 635 MB zram devices are untouched. `fstab` is
unchanged, so it survives reboot. `vm.swappiness` stays at 10.

The reasoning is about failure modes, not disk. GIAP's budgets derive from RAM
(`LLM_BUDGET_MB`, `jetson_context_size`) and never consult swap, so swap is
hidden slack: a model whose KV does not fit does not fail, it gets served from
swap and reads as a *slow model*. zram has the higher priority and fills first,
so the file only engages once 3.7 GB is already swapped — i.e. only in the
thrash regime. 2 GB keeps a cushion for a build spike while making a runaway
fail in minutes rather than hours; an 8 GB file previously let one consume
11.3 GB before dying.

For a cold CUDA build, add a temporary `/swapfile.build` and remove it after.
Never put it in `fstab`.

## 3. Headless — the finding

The default target was **already** `multi-user.target`, yet `gdm3` was running.
Stopping it:

| | before | after | delta |
|---|---:|---:|---:|
| RAM available | 2,219 MiB | 6,831 MiB | +4,612 MiB |
| **nvmap (GPU)** | **2,594 MiB** | **0 MiB** | **−2,594 MiB** |

**The two figures are not the same claim.** The available-RAM delta spans both
the display stack going away *and* a `drop_caches` flush that ran between the
two audits, so quoting +4,612 MiB as "headless" would credit it with the flush
(the flush alone reported +1,075 MiB). The nvmap delta is the clean one, and it
is the one that matters most: **the GNOME desktop was holding 2.6 GB of GPU
memory**, and NvMap contiguous allocation is exactly what gates whether E4B can
fully offload on this board.

That is 3× what `jetson-headless-mode` predicts. Its savings table
(`disable-graphical-target` "up to 865 MB") counts process RSS only — Xorg
75 MB, gnome-shell 186 MB here — and has no notion of nvmap at all. On a
unified-memory board the RSS is the small half.

Durable: `gdm3` has an empty `WantedBy`, only `graphical.target` wants
`display-manager.service`, and the default target is `multi-user.target` — so
it does not come back at boot. Note that `systemctl disable display-manager.service`
is a no-op here and *prints success*: gdm3 provides that name as an alias, so
there is no separate unit file. The default target is what holds it down.

Kept deliberately: `nvargus-daemon` (camera), `nvgetty` (serial recovery),
`avahi-daemon` (`nano.local`, which `deploy.sh` resolves), `bluetooth` (Matter),
`pulseaudio` (voice).

## 4. The custom `nvmap.ko` is now protected

It is the **only** nvmap on this box — there is no stock module in the
`kernel/` tree — and it is what lifts the r36.4.7 CVE-2025-33182 large-allocation
cap that otherwise makes E4B fall back to partial offload with NvMap error 12.

`dpkg -S` finds **no owning package** for it, and before this work
`apt-mark showhold` was **empty**: nothing protected it. Now held:
`nvidia-l4t-kernel`, `-dtbs`, `-headers`, `-oot-headers`, `-oot-modules`.

A kernel update or a reflash still removes it by construction. That is the
standing cost of staying on 6.2.1, and it is the first half of the JetPack
question — the other half being that NVIDIA's own fix for the same cap ships in
6.2.2 / r36.5.

`giap.sh doctor` names the state: `capped` is NVIDIA's signed module on an
affected release, `local` a module built on the board. To build one on a board
that cannot move to r36.5 yet, all on the device:

1. Fetch the r36.4.4 `public_sources.tbz2` from NVIDIA (it predates the cap) and
   extract the out-of-tree module sources.
2. Build `nvmap.ko` from them against the running kernel's headers.
3. Check its `vermagic` matches the running kernel and that the symbols `nvgpu`
   imports from it (`nvmap_dma_alloc_attrs`, `nvmap_dma_free_attrs`) carry the
   same CRCs as the stock module's. If either differs, stop.
4. Copy the stock module aside, install the new one over
   `/lib/modules/$(uname -r)/updates/drivers/video/tegra/nvmap/nvmap.ko`, run
   `depmod -a`, hold the five packages above, and reboot.

It reverts NVIDIA's hardening against CVE-2025-33182/33177, which keeps a large
GPU allocation out of the kernel's OOM path (a local denial of service), so it
is the board owner's call.

## What this does *not* change

`LLM_BUDGET_MB` is a compile-time constant (`7620 − 1500 OS − 200 STT −
100 TTS = 5820`), so the derived context sizes are unchanged. What changed is
the safety margin behind that constant: `SYSTEM_OVERHEAD_MB = 1500` was being
spent against a board that also had 2.6 GB of desktop GPU memory resident.
Whether the budget should now grow is a question for the bake-off's measured
peak footprints, not an assumption to make here.

## Power

Currently `MAXN_SUPER` (id 2), though `/etc/nvpmodel.conf` has `PM_CONFIG
DEFAULT=1` — `nvpmodel -m` persists to `/var/lib/nvpmodel/status` and is
replayed at boot, which is why the two disagree. Mode ids on this SKU:
`0=15W, 1=25W, 2=MAXN_SUPER, 3=7W`. **`-m 0` is the lowest mode, not MAXN.**

The production choice is deliberately left to the bake-off: prior measurement
put MAXN_SUPER at ~5 % more decode for 27× the over-current event rate, with
both modes clocking EMC at the LPDDR5 maximum of 3199 MHz. The harness records
the oc3 *rate* across each run so the comparison is made on this workload rather
than inherited.
