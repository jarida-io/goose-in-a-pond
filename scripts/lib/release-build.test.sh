#!/usr/bin/env bash
# Tests for release-build.sh. Run: bash scripts/lib/release-build.test.sh   (also under /bin/bash 3.2)
#
# Each test describes a host with fakes of uname, ld.lld, sccache and cargo first on PATH, a scratch
# HOME, and a scratch RELEASE_BUILD_ROOT standing in for /, then runs the real functions against it.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/release-build-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
SCRATCH="$(cd "$SCRATCH" && pwd)" || exit 1
trap 'rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"
PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

fake() { # fake <dir> <name> <body>: an executable script
  printf '#!/bin/sh\n%s\n' "$3" > "$1/$2"
  chmod +x "$1/$2"
}

# host <Linux|Darwin> <jetson yes|no> <ld.lld yes|no>
host() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/root/etc" "$W/bin" "$W/home/.cargo/bin"
  export PATH="$W/bin:$ORIG_PATH" HOME="$W/home" RELEASE_BUILD_ROOT="$W/root"
  unset RUSTFLAGS
  fake "$W/bin" uname "[ \"\$1\" = -s ] && echo $1"
  [ "$2" = yes ] && echo '# R36 (release), REVISION: 4.3' > "$W/root/etc/nv_tegra_release"
  [ "$3" = yes ] && fake "$W/bin" ld.lld 'exit 0'
  # Records what the build would have been given; every build in these tests is this one.
  fake "$W/home/.cargo/bin" cargo "{
  echo \"args=\$*\"
  echo \"RUSTFLAGS=\${RUSTFLAGS-unset}\"
  echo \"CMAKE_CUDA_ARCHITECTURES=\${CMAKE_CUDA_ARCHITECTURES-unset}\"
  echo \"SQLX_OFFLINE=\${SQLX_OFFLINE-unset}\"
  echo \"RUSTC_WRAPPER=\${RUSTC_WRAPPER-unset}\"
} > '$W/cargo-call'"
  # shellcheck source=release-build.sh
  source "$HERE/release-build.sh"
}

called() { grep "^$1=" "$W/cargo-call" | head -1 | cut -d= -f2-; }

test_jetson_flags() {
  host Linux yes yes
  eq "$(release_build_rustflags)" "-C target-cpu=native -C link-arg=-fuse-ld=lld -C codegen-units=8"
}

test_mac_keeps_only_native() {
  host Darwin no yes
  eq "$(release_build_rustflags)" "-C target-cpu=native"
}

test_linux_without_ld_lld() {
  host Linux no no
  eq "$(release_build_rustflags)" "-C target-cpu=native"
}

test_callers_rustflags_come_after_native() {
  host Linux yes no
  RUSTFLAGS="-C target-cpu=generic"
  eq "$(release_build_rustflags)" "-C target-cpu=native -C target-cpu=generic -C codegen-units=8"
}

test_flags_lead_with_cargo_config() {
  host Linux yes yes
  local config
  config="$(sed -n 's/^rustflags = \[\(.*\)\]$/\1/p' "$REPO/.cargo/config.toml" | tr -d '",')"
  [ -n "$config" ] || { echo "no build.rustflags in .cargo/config.toml"; return 1; }
  case "$(release_build_rustflags)" in "$config"*) ;; *) echo "[$config] does not lead [$(release_build_rustflags)]"; return 1 ;; esac
}

test_cuda_build_runs_with_the_flags() {
  host Linux yes yes
  bash -c "$(release_build_cmd "$RELEASE_BUILD_CUDA_FEATURES")" || return 1
  eq "$(called args)" "build -p pond-server --bin pond-server --features $RELEASE_BUILD_CUDA_FEATURES --release" || return 1
  eq "$(called RUSTFLAGS)" "$(release_build_rustflags)" || return 1
  eq "$(called CMAKE_CUDA_ARCHITECTURES)" 87 || return 1
  eq "$(called SQLX_OFFLINE)" true || return 1
  eq "$(called RUSTC_WRAPPER)" unset
}

test_cpu_build_has_no_cuda_settings() {
  host Darwin no no
  local cmd; cmd="$(release_build_cmd "")"
  lacks "$cmd" CMAKE_CUDA_ARCHITECTURES || return 1
  lacks "$cmd" /usr/local/cuda || return 1
  bash -c "$cmd" || return 1
  eq "$(called args)" "build -p pond-server --bin pond-server --release" || return 1
  eq "$(called RUSTFLAGS)" "-C target-cpu=native"
}

test_sccache_wraps_rustc_when_present() {
  host Linux no no
  fake "$W/bin" sccache 'exit 0'
  bash -c "$(release_build_cmd "")" || return 1
  eq "$(called RUSTC_WRAPPER)" sccache
}

test_stamp_records_the_flags() {
  host Linux yes yes
  local stamp; stamp="$(release_build_stamp "$RELEASE_BUILD_CUDA_FEATURES" b1ce706e 9e7c4fb1e built)"
  eq "$(release_build_stamp_field "$stamp" rustflags)" "-C,target-cpu=native,-C,link-arg=-fuse-ld=lld,-C,codegen-units=8" || return 1
  eq "$(release_build_stamp_field "$stamp" rustflags)" "$(release_build_stamp_rustflags)" || return 1
  eq "$(release_build_stamp_field "$stamp" git)" b1ce706e || return 1
  eq "$(release_build_stamp_field "$stamp" goose)" 9e7c4fb1e || return 1
  eq "$(release_build_stamp_field "$stamp" cuda_arch)" 87 || return 1
  eq "$(release_build_stamp_field "$stamp" features)" "$RELEASE_BUILD_CUDA_FEATURES"
}

test_old_stamp_does_not_match() {
  host Linux yes yes
  local old="features=x cuda_arch=87 rustflags=inherited git=b1ce706e goose=9e7c4fb1e ui=built built=2026-11-07T13:27:44Z"
  eq "$(release_build_stamp_field "$old" git)" b1ce706e || return 1
  [ "$(release_build_stamp_field "$old" rustflags)" != "$(release_build_stamp_rustflags)" ] || { echo "inherited matched"; return 1; }
  eq "$(release_build_stamp_field "no stamp here" git)" ""
}

# deploy.sh sends the functions it names to the device, where nothing has sourced the library.
test_deploy_sends_every_function_it_needs() {
  host Linux yes yes
  local names here there
  names="$(tr '\n' ' ' < "$REPO/scripts/jetson/deploy.sh" | sed -n 's/.*\$(declare -f \([^)]*\)).*/\1/p' | tr -d '\\')"
  [ -n "$names" ] || { echo "deploy.sh no longer sends functions with declare -f"; return 1; }
  # shellcheck disable=SC2086 # one name per word
  here="$(release_build_cmd x/cuda; release_build_stamp x/cuda a b built | sed 's/ built=.*//')"
  # shellcheck disable=SC2086
  there="$(env -i PATH="$PATH" HOME="$HOME" RELEASE_BUILD_ROOT="$RELEASE_BUILD_ROOT" bash -c "$(declare -f $names)
release_build_cmd x/cuda; release_build_stamp x/cuda a b built | sed 's/ built=.*//'" 2>&1)"
  eq "$there" "$here"
}

# Every release build of pond-server goes through release_build_cmd; one that builds its own way
# rebuilds everything when a pond switches to it, and may lose target-cpu=native.
test_every_build_path_uses_it() {
  local f
  for f in scripts/giap.sh scripts/lib/install-build.sh scripts/jetson/build-native.sh scripts/jetson/deploy.sh; do
    contains "$(cat "$REPO/$f")" release_build_cmd || { echo "in $f"; return 1; }
    lacks "$(cat "$REPO/$f")" "cargo build -p pond-server" || { echo "in $f"; return 1; }
    lacks "$(cat "$REPO/$f")" "RUSTFLAGS=" || { echo "in $f"; return 1; }
  done
}

t "a Jetson builds native, links with lld and uses eight codegen units" test_jetson_flags
t "a Mac keeps only target-cpu=native" test_mac_keeps_only_native
t "Linux without ld.lld links with the default linker" test_linux_without_ld_lld
t "the caller's RUSTFLAGS follow target-cpu=native, so they can override it" test_callers_rustflags_come_after_native
t ".cargo/config.toml's rustflags lead the list" test_flags_lead_with_cargo_config
t "a CUDA build runs cargo with the flags, sm_87 and the features" test_cuda_build_runs_with_the_flags
t "a CPU build sets nothing for CUDA" test_cpu_build_has_no_cuda_settings
t "sccache wraps rustc when installed" test_sccache_wraps_rustc_when_present
t "the stamp records the flags as one word" test_stamp_records_the_flags
t "a stamp from before the shared flags does not match" test_old_stamp_does_not_match
t "deploy.sh sends every function the device needs" test_deploy_sends_every_function_it_needs
t "every build path goes through release_build_cmd" test_every_build_path_uses_it

printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
