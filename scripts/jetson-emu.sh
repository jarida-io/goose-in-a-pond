#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# jetson-emu.sh — run GIAP on this Mac as if it were the Jetson.
#
#   bash scripts/jetson-emu.sh run [--profile P] [-- CMD...]   tier 1: native Mac
#   bash scripts/jetson-emu.sh test [--profile P] [FILTER]     tier 1: cargo test
#   bash scripts/jetson-emu.sh box [--profile P] [-- CMD...]   tier 2: arm64 container
#   bash scripts/jetson-emu.sh profiles                        list the device profiles
#   bash scripts/jetson-emu.sh doctor                          what this can and cannot do
#
# ── What this is ─────────────────────────────────────────────────────────────
#
# The Jetson-specific behaviour in this workspace is a handful of independent
# reads — /proc/device-tree/model, /etc/nv_tegra_release, a RAM constant, and a
# cargo feature — and none of them is true on a MacBook. The consequence is not
# that the Mac behaves a little differently. It is that whole branches are
# UNREACHABLE there:
#
#   LocalInferenceLlmAdapter::apply_jetson_settings is #[cfg(feature = "cuda")],
#   so jetson_context_size — the arithmetic that can OOM the board — had unit
#   tests for its answer and no caller on any machine a developer can run.
#
# This runs those branches on the Mac. See
# crates/pond-core/src/models/domain/device_profile.rs.
#
# ── What this is NOT ─────────────────────────────────────────────────────────
#
# It emulates what the board DECIDES, never what the board SURVIVES.
#
#   * Tier 1 cannot enforce a memory ceiling. macOS has no cgroups; a 64 GB Mac
#     will not run out of memory at 7,620 MB however convincingly it reports it.
#   * NO TIER is a source of a performance or KV-cost number. The Mac has
#     already reported a THIRD of the device's real per-token KV cost for the
#     same model, and a commit that believed it was wrong. Throughput, TTFT,
#     KV cost, NvMap behaviour and live free memory come from the Orin.
#   * Neither tier has CUDA. Tier 2 is a generic arm64 container with no JetPack.
#
# `doctor` prints this table with the specific call sites. Read it once.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

PROFILE="orin-nano-8gb"
DATA_DIR=""
MEMORY_MB=""
CMD="${1:-help}"
shift || true

CMD_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="${2:?--profile needs a name}"; shift 2 ;;
    --profile=*) PROFILE="${1#*=}"; shift ;;
    --data)    DATA_DIR="${2:?--data needs a path}"; shift 2 ;;
    --memory)  MEMORY_MB="${2:?--memory needs MB}"; shift 2 ;;
    --)        shift; CMD_ARGS=("$@"); break ;;
    *)         CMD_ARGS+=("$1"); shift ;;
  esac
done

say()  { printf '\n=== %s ===\n' "$1"; }
note() { printf '  %s\n' "$1"; }
die()  { printf '\n!! %s\n' "$1" >&2; exit 1; }

# ── Profiles ─────────────────────────────────────────────────────────────────
#
# One line per canonical profile: NAME:TOTAL_RAM_MB.
#
# This IS a duplication of DeviceProfile::builtin in
# crates/pond-core/src/models/domain/device_profile.rs, and it is deliberate —
# the shell needs the RAM figure before any Rust has run, to size the container.
# It is guarded rather than eliminated: `shell_and_rust_agree_about_the_profiles`
# in device_profile.rs reads THIS table and fails the build if the two drift, so
# the duplication cannot rot silently.
#
# Canonical names only. The Rust also accepts aliases (orin-nano, jetson,
# orin-nx) for anyone exporting POND_DEVICE_PROFILE by hand; this script does
# not, so that its table stays exactly parallel to DeviceProfile::builtin_names.
EMU_PROFILES="orin-nano-8gb:7620 orin-nano-8gb-cpu:7620 orin-nx-16gb:15564"

profile_default_memory_mb() {
  local want="$1" entry
  for entry in $EMU_PROFILES; do
    [ "${entry%%:*}" = "$want" ] && { echo "${entry##*:}"; return 0; }
  done
  echo ""
}

list_profile_names() {
  local entry
  for entry in $EMU_PROFILES; do printf '%s ' "${entry%%:*}"; done
}

# Fail on an unknown profile HERE rather than letting the binary log an error
# and run as the host. A run that silently declined to emulate is worse than one
# that refused to start, because its green result means nothing and says so
# nowhere.
assert_known_profile() {
  [ -n "$(profile_default_memory_mb "$PROFILE")" ] || die \
    "unknown profile '$PROFILE'. Known: $(list_profile_names)
   Add it to DeviceProfile::builtin in crates/pond-core/src/models/domain/device_profile.rs
   and to EMU_PROFILES in this script (a test asserts the two agree)."
}

# ── Scratch data dir ─────────────────────────────────────────────────────────
#
# Defaults to a scratch pond so an emulated run can never touch the real one.
#
# It deliberately does NOT symlink the real models/ directory in, however
# tempting: pond-server's startup migration MOVES GGUFs into the data dir it is
# given, so a symlinked scratch pond eats the real model collection and takes it
# with it when the scratch dir is removed. Point GIAP_GGUF_MODEL_PATH at a file
# outside the data dir instead — that path is read, never relocated.
setup_data_dir() {
  if [ -z "$DATA_DIR" ]; then
    DATA_DIR="${TMPDIR:-/tmp}/pond-jetson-emu-$$"
    SCRATCH=1
  else
    SCRATCH=0
  fi
  mkdir -p "$DATA_DIR"
}

cleanup() {
  if [ "${SCRATCH:-0}" -eq 1 ] && [ -n "${DATA_DIR:-}" ]; then
    rm -rf "$DATA_DIR"
  fi
}
trap cleanup EXIT

emu_env() {
  printf 'POND_DEVICE_PROFILE=%s\n' "$PROFILE"
  [ -n "$MEMORY_MB" ] && printf 'POND_DEVICE_TOTAL_RAM_MB=%s\n' "$MEMORY_MB"
  return 0
}

# ── Commands ─────────────────────────────────────────────────────────────────

cmd_run() {
  assert_known_profile
  setup_data_dir
  say "Tier 1 — native Mac, emulating $PROFILE"
  note "data dir : $DATA_DIR $([ "$SCRATCH" -eq 1 ] && echo '(scratch, removed on exit)')"
  note "profile  : $PROFILE"
  note "NOT emulated: memory ceiling, CUDA, aarch64 codegen, KV cost, throughput."
  note "Do not read a performance number out of this run."

  local -a run_cmd
  if [ ${#CMD_ARGS[@]} -gt 0 ]; then
    run_cmd=("${CMD_ARGS[@]}")
  else
    run_cmd=(cargo run -p pond-server --features local-inference -- serve)
  fi

  local -a emu=() kv
  while IFS= read -r kv; do emu+=("$kv"); done < <(emu_env)

  say "Running: ${run_cmd[*]}"
  env "${emu[@]}" \
      POND_DATA_DIR="$DATA_DIR" \
      POND_DEV_ALLOW_LOOPBACK=1 \
      RUST_LOG="${RUST_LOG:-info}" \
      "${run_cmd[@]}"
}

cmd_test() {
  assert_known_profile
  say "Tier 1 — cargo test under profile $PROFILE"
  note "This runs the workspace's own tests with the device branches LIVE."
  note "A test that passes here and fails on the board is a fidelity gap; log it."

  # -p is limited to the crates whose behaviour a profile actually changes.
  # Running the whole workspace under emulation would mostly prove that most of
  # it does not consult the profile, which is true by construction.
  local -a filter=()
  [ ${#CMD_ARGS[@]} -gt 0 ] && filter=("${CMD_ARGS[@]}")
  local -a emu=() kv
  while IFS= read -r kv; do emu+=("$kv"); done < <(emu_env)

  env "${emu[@]}" \
    cargo test -p pond-core -p pond-adapters-local-inference "${filter[@]:-}"
}

cmd_box() {
  assert_known_profile
  command -v docker >/dev/null 2>&1 || die "docker is not on PATH; the container tier needs it."

  local mem="${MEMORY_MB:-$(profile_default_memory_mb "$PROFILE")}"
  say "Tier 2 — linux/arm64 container, emulating $PROFILE"
  note "memory ceiling : ${mem} MB (ENFORCED by the container runtime — this one is real)"
  note "architecture   : aarch64 Linux, native on Apple Silicon (no qemu)"
  note "NOT emulated   : CUDA, JetPack, NvMap, the Orin's memory bandwidth."
  note ""
  note "This tier exists for the two things tier 1 cannot do: run real aarch64"
  note "codegen (the class of bug where kokoro's q8f16 returns digital silence on"
  note "the board and is fine on macOS) and actually fail when the board would."

  # /etc/nv_tegra_release is a real file here, so the container reaches the
  # JetPack branch of host_is_accelerated through the same code path the device
  # does rather than through the profile. The device tree cannot be faked --
  # Docker will not mount into /proc -- which is exactly the container case
  # `a_container_without_a_device_tree_is_still_recognised_by_jetpack` covers.
  local -a box_cmd
  if [ ${#CMD_ARGS[@]} -gt 0 ]; then
    box_cmd=("${CMD_ARGS[@]}")
  else
    box_cmd=(cargo test -p pond-core -p pond-adapters-local-inference)
  fi

  # The box keeps its own cargo caches between runs, and copies in this Mac's git checkouts so the
  # private llama-cpp-rs-giap resolves with no credentials in the container.
  local host_git="${CARGO_HOME:-$HOME/.cargo}/git"
  local -a seed=()
  [ -d "$host_git/db" ] && seed=(-v "$host_git":/host-cargo-git:ro)

  say "Running in container: ${box_cmd[*]}"
  docker run --rm --platform linux/arm64 \
    --memory "${mem}m" --memory-swap "${mem}m" \
    -v "$REPO_ROOT":/src -w /src \
    -v giap-jetson-emu-registry:/usr/local/cargo/registry \
    -v giap-jetson-emu-git:/usr/local/cargo/git \
    ${seed[@]+"${seed[@]}"} \
    -e CARGO_TARGET_DIR=/src/target-jetson \
    -e RUSTFLAGS="-C target-cpu=cortex-a78" \
    -e CFLAGS="-march=armv8.2-a+fp16+dotprod" \
    -e CXXFLAGS="-march=armv8.2-a+fp16+dotprod" \
    -e CMAKE_TOOLCHAIN_FILE=/src/scripts/jetson/ggml-toolchain.cmake \
    -e SQLX_OFFLINE=true -e CARGO_TERM_COLOR=never \
    -e POND_DEVICE_PROFILE="$PROFILE" \
    ${MEMORY_MB:+-e POND_DEVICE_TOTAL_RAM_MB="$MEMORY_MB"} \
    -e RUST_LOG="${RUST_LOG:-info}" \
    rust:bookworm bash -c '
      set -e
      apt-get update -qq
      apt-get install -y -qq cmake pkg-config libssl-dev libasound2-dev \
        libdbus-1-dev libsqlite3-dev clang git >/dev/null 2>&1
      rustup component add rustfmt clippy >/dev/null 2>&1
      if [ -d /host-cargo-git/db ]; then
        mkdir -p /usr/local/cargo/git/db
        for db in /host-cargo-git/db/*; do
          [ -e "/usr/local/cargo/git/db/${db##*/}" ] || cp -a "$db" /usr/local/cargo/git/db/
        done
      fi
      # Make the JetPack marker real, so the acceleration probe reaches the
      # container branch through the filesystem and not through the profile.
      echo "# R36 (release), REVISION: 4.3, GCID: 00000000, BOARD: generic" \
        > /etc/nv_tegra_release
      echo "  arch: $(uname -m)  ram-limit: $(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo unknown)"
      '"$(printf '%q ' "${box_cmd[@]}")"'
    '
}

cmd_profiles() {
  say "Device profiles"
  note "Defined in crates/pond-core/src/models/domain/device_profile.rs"
  note ""
  local entry
  for entry in $EMU_PROFILES; do
    printf '  %-20s %s MB\n' "${entry%%:*}" "${entry##*:}"
  done
  note ""
  note "Aliases (Rust-side only): orin-nano, jetson, orin-nano-cpu, orin-nx"
}

cmd_doctor() {
  say "What jetson-emu can and cannot emulate"
  cat <<'TABLE'

  EMULATED (tier 1, native Mac) — the decisions the board takes
    acceleration probe            crates/pond-server/src/main.rs        report_acceleration
    registry model settings       pond-adapters-local-inference/lib.rs  apply_model_settings
    context-window derivation     pond-adapters-local-inference/lib.rs  jetson_context_size
    LLM memory budget             pond-adapters-local-inference/        scheduler::llm_budget_mb
    device RAM reporting          pond-adapters-local-inference/        scheduler::total_ram_mb

  EMULATED (tier 2, arm64 container) — additionally
    aarch64 codegen               real, native on Apple Silicon
    memory ceiling                real, enforced by the container runtime
    /proc, /etc/nv_tegra_release  real Linux
    linux-aarch64 platform key    model_record::current_platform_key
    aarch64 kokoro tier default   pond-adapters-kokoro::aarch64_linux

  NOT EMULATED — anywhere. Get these from the Orin.
    KV cost per token             the Mac has reported a THIRD of the real cost
    prefill / decode throughput   the whole point of the board
    TTFT                          ditto
    CUDA kernels, NvMap, sm_87    no CUDA in either tier
    NvMap single-allocation wall  ~586 MiB, invisible off-device
    live free memory              /proc/meminfo is the host's in tier 2
    swap behaviour                the Mac swaps differently and silently

  THE RULE
    An emulated run answers "would the Orin DECIDE this?".
    It never answers "would the Orin SURVIVE this?".
    Develop on the Mac, confirm on the device, in that order.

TABLE
  say "Environment"
  note "POND_DEVICE_PROFILE       name of the profile, or unset/none/host to be honest"
  note "POND_DEVICE_TOTAL_RAM_MB  override the profile's RAM figure"
  note "POND_DEVICE_PRETEND_CUDA  0 forces the CPU-build-on-a-Jetson warning path"
  say "Sanity"
  if [ -z "${POND_DEVICE_PROFILE:-}" ]; then
    note "OK   POND_DEVICE_PROFILE is not set in this shell — nothing is emulated by accident."
  else
    note "WARN POND_DEVICE_PROFILE=$POND_DEVICE_PROFILE is set IN YOUR SHELL."
    note "     Every cargo run and cargo test from here emulates. Unset it when you are done."
  fi
  command -v docker >/dev/null 2>&1 \
    && note "OK   docker present — the container tier is available." \
    || note "MISS docker not on PATH — tier 2 (box) unavailable."
}

case "$CMD" in
  run)      cmd_run ;;
  test)     cmd_test ;;
  box)      cmd_box ;;
  profiles) cmd_profiles ;;
  doctor)   cmd_doctor ;;
  help|--help|-h) sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
  *) die "unknown command: $CMD (try: run | test | box | profiles | doctor)" ;;
esac
