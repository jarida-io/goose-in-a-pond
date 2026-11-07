#!/usr/bin/env bash
# The release build of pond-server on the machine that will run it. giap.sh, install.sh,
# scripts/jetson/build-native.sh and, on the device, scripts/jetson/deploy.sh all build through
# release_build_cmd. Tested by release-build.test.sh.
#
# They must agree exactly: Cargo hashes RUSTFLAGS into every crate, so a pond built one way and then
# another recompiles everything, llama.cpp's and whisper.cpp's CUDA kernels included.
#
# RELEASE_BUILD_ROOT stands in for / in tests.

# whisper/cuda moves speech recognition onto the GPU: ~20 s of audio in 1-2.6 s, not a burst on all six cores.
# shellcheck disable=SC2034  # read by the scripts that source this
RELEASE_BUILD_CUDA_FEATURES="pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda"

release_build_is_jetson() { [ -f "${RELEASE_BUILD_ROOT:-}/etc/nv_tegra_release" ]; }

# A RUSTFLAGS variable replaces .cargo/config.toml's build.rustflags instead of adding to it, so
# target-cpu=native comes first here: without it llama-cpp-sys-2 builds ggml's CPU code for armv8-a.
release_build_rustflags() {
  local flags="-C target-cpu=native"
  [ -n "${RUSTFLAGS:-}" ] && flags="$flags $RUSTFLAGS"
  # gcc's -fuse-ld=lld runs ld.lld, which a bare `lld` on PATH does not provide.
  if [ "$(uname -s)" = Linux ] && command -v ld.lld >/dev/null 2>&1; then
    flags="$flags -C link-arg=-fuse-ld=lld"
  fi
  # The profile's single codegen unit leaves most of the Orin's six cores idle on the largest crates.
  if release_build_is_jetson; then flags="$flags -C codegen-units=8"; fi
  printf '%s\n' "$flags"
}

# The build as one shell line, for `bash -c` or eval where it runs. $1: cargo features, or empty.
release_build_cmd() {
  local features="${1:-}" env wrapper=""
  command -v sccache >/dev/null 2>&1 && wrapper="RUSTC_WRAPPER=sccache "
  case "$features" in
    # sm_87 makes ggml ship a real Orin cubin, not compute_80 PTX the driver JIT-compiles at first load.
    *cuda*) env="PATH=\"\$HOME/.cargo/bin:/usr/local/cuda/bin:\$PATH\" CMAKE_CUDA_ARCHITECTURES=87 CUDA_COMPUTE_CAP=87 " ;;
    *) env="PATH=\"\$HOME/.cargo/bin:\$PATH\" " ;;
  esac
  printf "%sRUSTFLAGS='%s' %sSQLX_OFFLINE=true cargo build -p pond-server --bin pond-server%s --release\n" \
    "$env" "$(release_build_rustflags)" "$wrapper" "${features:+ --features $features}"
}

# The flags as target/release/.giap-build-stamp records them: one word, so the stamp stays key=value.
release_build_stamp_rustflags() {
  local flags
  flags="$(release_build_rustflags)"
  printf '%s\n' "${flags// /,}"
}

# The stamp line. $1 features, $2 commit, $3 goose commit, $4 the web UI's state.
release_build_stamp() {
  local arch=unset
  case "$1" in *cuda*) arch=87 ;; esac
  printf 'features=%s cuda_arch=%s rustflags=%s git=%s goose=%s ui=%s built=%s\n' \
    "$1" "$arch" "$(release_build_stamp_rustflags)" "$2" "$3" "$4" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
}

# One field of a stamp line, or nothing.
release_build_stamp_field() {
  printf ' %s\n' "$1" | sed -n "s/.* $2=\([^ ]*\).*/\1/p"
}
