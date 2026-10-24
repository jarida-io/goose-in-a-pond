#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# litert-setup.sh — build, package and verify Google's LiteRT-LM C API library,
# which goose's `litert` backend opens at runtime for .litertlm models.
#
# Sourced, never run: it defines constants and functions and does nothing else.
# Used by scripts/giap.sh (`giap.sh litert`, the banner and doctor), by
# scripts/stage-server-sidecar.sh, scripts/verify-sidecar.sh and
# scripts/jetson/deploy.sh, and inside the linux-arm64 build container.
#
# A package is one directory per platform, here or wherever it is copied:
#
#   ~/.giap/litert-lm/<capi>-<commit8>/<platform>/
#     liblitert-lm.{dylib,so}   the C API, built from the pinned commit
#     lib*.{dylib,so}           LiteRT's prebuilt runtime, accelerators and samplers
#     include/*.h               the public C headers
#     LICENSE
#     MANIFEST.sha256           line 1 describes the build, then "<sha256>  ./<path>" per file
#
# ~/.giap/litert-lm/.path-<platform> records the last package that was built and verified.
#
# Two kinds of function. The data functions (paths, litert_package, litert_write_manifest,
# litert_verify, litert_stage_desktop) print nothing on stdout and work under
# `set -euo pipefail`, which is how the stage, verify and deploy scripts run. The rest
# (litert_prepare_source, litert_build, litert_status) talk to a person through the
# caller's say/ok/warn/bad/info/note/run, as giap.sh and the tests define them.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`,
# no `${x,,}`, and no expansion of an array that may be empty.
# ─────────────────────────────────────────────────────────────────────────────

LITERT_REPO_URL="https://github.com/jarida-io/LiteRT-LM.git"
# The pin is giap-main on GIAP's fork: LiteRT-LM main (b320801b, C API 1.0.0, in no release yet) plus
# GIAP's fixes, among them fp32 activations on the GPU (fp16 garbled tool calls), KV snapshots and
# KV reuse through an unchanged image. docs/developer/litert-lm.md says how to move it.
LITERT_COMMIT="a5dd27b5bd363be38feb62af637facbdb043b11c"
LITERT_CAPI_VERSION="1.0.0"
LITERT_TARGET="//c:litert-lm"
LITERT_HEADERS="api_export.h engine.h conversation.h model_info.h error_reporter.h experimental.h embedding_engine.h"
# In every package. liblitert-lm links the next two; the GPU path on both platforms (WebGPU/Dawn,
# over Metal on a Mac and Vulkan on the Orin) needs the other three. The macOS prebuilts add Metal ones.
LITERT_REQUIRED_LIBS="liblitert-lm libLiteRt libGemmaModelConstraintProvider libLiteRtWebGpuAccelerator libLiteRtTopKWebGpuSampler libwebgpu_dawn"
# The desktop app runs on Metal. LiteRT's loader tries the WebGPU accelerator before Metal's on a
# Mac and keeps the first it finds, so the app leaves out WebGPU and Dawn, which only WebGPU loads.
# On an M4 with E2B, Metal measured warm turns at 129-142 ms to the first token against 155-192, and
# decode about 13% faster. A package keeps both: a pond run from it on a Mac runs on WebGPU, as the
# Orin does, unless GIAP_LITERT_ACCELERATOR=metal (crates/pond-server/src/litert_runtime.rs).
LITERT_DESKTOP_LEAVES_OUT="libLiteRtWebGpuAccelerator libLiteRtTopKWebGpuSampler libwebgpu_dawn"
# shellcheck disable=SC2034  # read by the scripts that source this file
LITERT_DESKTOP_REQUIRED_LIBS="liblitert-lm libLiteRt libGemmaModelConstraintProvider libLiteRtMetalAccelerator libLiteRtTopKMetalSampler"

# linux-arm64 builds in this image on any Docker host that runs linux/arm64: glibc 2.35, as on JetPack 6.
LITERT_LINUX_IMAGE="ubuntu:22.04"
# The clang major the Orin built this tree with. LiteRT-LM's .bazelrc calls plain `clang` on Linux.
LITERT_LINUX_CLANG=15
LITERT_BAZELISK_VERSION="v1.29.0"
# The asset digest GitHub publishes for bazelisk-linux-arm64 in that release.
LITERT_BAZELISK_LINUX_ARM64_SHA256="e20e8b0f4f240091b7a55bf17b9398bd4f40ee70ae0208dff95dd4c445fb4010"
# Bazel's output root and bazelisk's downloads persist in this Docker volume between builds.
LITERT_DOCKER_VOLUME="giap-litert-lm-linux-arm64"
# What a linux-arm64 package may take from the device instead of carrying: glibc and the GCC runtime.
LITERT_LINUX_SYSTEM_LIBS="libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 librt.so.1 ld-linux-aarch64.so.1 libgcc_s.so.1 libstdc++.so.6"

LITERT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_LITERT_NL='
'

# ── where things live ────────────────────────────────────────────────────────

# Overridable so a test never touches the real home directory.
litert_home()        { printf '%s' "${GIAP_LITERT_HOME:-$HOME/.giap/litert-lm}"; }
litert_tag()         { printf '%s-%s' "$LITERT_CAPI_VERSION" "$(printf '%s' "$LITERT_COMMIT" | cut -c1-8)"; }
litert_package_dir() { printf '%s/%s/%s' "$(litert_home)" "$(litert_tag)" "$1"; }
litert_record_file() { printf '%s/.path-%s' "$(litert_home)" "$1"; }
# LITERT_LM_SRC names a clone of your own; otherwise this tool keeps one.
litert_src_dir()     { printf '%s' "${LITERT_LM_SRC:-$(litert_home)/src}"; }
# Where scripts/jetson/deploy.sh puts the linux-arm64 package, under the pond's data directory.
litert_device_subdir() { printf 'lib/litert-lm/%s' "$(litert_tag)"; }

litert_recorded_dir() {
  local f; f="$(litert_record_file "$1")"
  if [ -f "$f" ]; then sed -n 1p "$f"; fi
  return 0
}
litert_record() {
  local f; f="$(litert_record_file "$1")"
  mkdir -p "$(dirname "$f")" || return 1
  printf '%s\n' "$2" > "$f"
}

# ── platforms ────────────────────────────────────────────────────────────────

litert_platform_ok() { case "$1" in macos-arm64|linux-arm64) return 0 ;; esac; return 1; }
# The Bazel --config, which is also the name of LiteRT's prebuilt directory.
_litert_config() { case "$1" in macos-arm64) printf 'macos_arm64' ;; linux-arm64) printf 'linux_arm64' ;; esac; }
_litert_ext()    { case "$1" in macos-arm64) printf 'dylib' ;; linux-arm64) printf 'so' ;; esac; }
# What `giap.sh litert build` builds when no platform is named.
litert_default_platform() {
  case "$(uname -s)/$(uname -m)" in Darwin/arm64) printf 'macos-arm64' ;; *) printf 'linux-arm64' ;; esac
}

# How <platform> builds on this host: `native` with the host's own Bazel, or `docker`. linux-arm64
# builds natively on an arm64 Linux host, the Jetson itself, and in a container anywhere else.
litert_build_mode() {
  case "$1/$(uname -s)/$(uname -m)" in
    linux-arm64/Linux/aarch64) printf 'native' ;;
    linux-arm64/*) printf 'docker' ;;
    *) printf 'native' ;;
  esac
}

# The flags that decide what Bazel builds. The manifest's first line records exactly these.
# A native linux-arm64 build, on the Jetson, uses the flags the board's own builds have always
# used and every Orin measurement was made with: no --config=linux_arm64 (-march=armv8-a, -O3,
# YNNPACK), which would also recompile every target there, for hours, and no header parsing.
litert_bazel_flags() { # <platform> [mode]
  if [ "$1" = linux-arm64 ] && [ "${2:-$(litert_build_mode "$1")}" = native ]; then
    printf -- '-c opt --define=litert_runtime_link_mode=dynamic --features=-parse_headers'
  else
    printf -- '-c opt --config=%s --define=litert_runtime_link_mode=dynamic' "$(_litert_config "$1")"
  fi
}

litert_manifest_header() { # <platform> <commit> [bazel flags]
  printf 'litert-lm C API %s | LiteRT-LM %s | built %s on %s | bazel %s %s' \
    "$LITERT_CAPI_VERSION" "$2" "$(date -u '+%Y-%m-%dT%H:%MZ')" "$1" "${3:-$(litert_bazel_flags "$1")}" "$LITERT_TARGET"
}
_litert_header_capi()     { printf '%s\n' "$1" | sed -n 's/^litert-lm C API \([^ ]*\) |.*/\1/p'; }
_litert_header_commit()   { printf '%s\n' "$1" | sed -n 's/.*| LiteRT-LM \([0-9a-f]\{40\}\) |.*/\1/p'; }
_litert_header_platform() { printf '%s\n' "$1" | sed -n 's/.* on \([a-z0-9-]*\) | bazel .*/\1/p'; }

# ── small tools ──────────────────────────────────────────────────────────────

_litert_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else return 1; fi
}

# Show a command, then run it: the caller's `run` when there is one, which also honours --dry-run.
_litert_run() {
  if declare -F run >/dev/null 2>&1; then run "$@"; return; fi
  printf '$ %s\n' "$*"
  "$@"
}

_litert_timeout() { # <seconds> <command...>
  local s="$1"; shift
  if command -v timeout >/dev/null 2>&1; then timeout "$s" "$@"
  elif command -v perl >/dev/null 2>&1; then perl -e 'alarm shift @ARGV; exec @ARGV or exit 127' "$s" "$@"
  else "$@"; fi
}

_litert_is_lfs_pointer() {
  case "$(head -c 64 "$1" 2>/dev/null | tr -d '\000')" in
    "version https://git-lfs.github.com/spec/"*) return 0 ;;
  esac
  return 1
}

# The LC_RPATH entries of a Mach-O file, one per line.
_litert_rpaths() {
  otool -l "$1" 2>/dev/null | awk '
    $1 == "cmd" && $2 == "LC_RPATH" { want = 1; next }
    want && $1 == "path" { sub(/^[ \t]*path[ \t]+/, ""); sub(/[ \t]+\(offset [0-9]+\)[ \t]*$/, ""); print; want = 0 }'
}

# NEEDED, RUNPATH and RPATH of an ELF file as "<KIND> <value>" lines, by readelf or by objdump,
# which on a Mac is llvm-objdump and reads ELF too. Fails when neither is installed.
_litert_elf_dynamic() {
  if command -v readelf >/dev/null 2>&1; then
    readelf -d "$1" 2>/dev/null | sed -n -e 's/.*(NEEDED).*\[\(.*\)\].*/NEEDED \1/p' \
      -e 's/.*(RUNPATH).*\[\(.*\)\].*/RUNPATH \1/p' -e 's/.*(RPATH).*\[\(.*\)\].*/RPATH \1/p'
  elif command -v objdump >/dev/null 2>&1; then
    objdump -p "$1" 2>/dev/null | awk '$1 == "NEEDED" || $1 == "RUNPATH" || $1 == "RPATH" { print $1, $2 }'
  else
    return 1
  fi
}

# ── packaging ────────────────────────────────────────────────────────────────

# Every dylib finds the others through @loader_path, and is signed again after the edit.
_litert_fix_macho() {
  local dir="$1" lib="$1/liblitert-lm.dylib" f rp
  _litert_run install_name_tool -id @rpath/liblitert-lm.dylib "$lib" || return 1
  # Bazel's rpaths point into its own sandbox (../_solib_darwin_arm64/...); none of them ship.
  for rp in $(_litert_rpaths "$lib"); do
    _litert_run install_name_tool -delete_rpath "$rp" "$lib" || return 1
  done
  _litert_run install_name_tool -add_rpath @loader_path "$lib" || return 1
  for f in "$dir"/*.dylib; do
    [ "$f" = "$lib" ] && continue
    case "$_LITERT_NL$(_litert_rpaths "$f")$_LITERT_NL" in
      *"${_LITERT_NL}@loader_path${_LITERT_NL}"*) ;;
      *) _litert_run install_name_tool -add_rpath @loader_path "$f" || return 1 ;;
    esac
  done
  # Apple Silicon refuses to load a Mach-O whose signature no longer matches its contents.
  for f in "$dir"/*.dylib; do
    _litert_run codesign --force -s - "$f" || return 1
  done
}

# The prebuilt RUNPATHs name Google-internal build paths; every library looks beside itself instead.
_litert_fix_elf() {
  local f
  for f in "$1"/*.so; do
    # shellcheck disable=SC2016
    _litert_run patchelf --set-rpath '$ORIGIN' "$f" || return 1
  done
}

# "<sha256>  ./<path>" for every file under <dir> except the manifest, after <header> as line 1.
litert_write_manifest() { # <dir> <header>
  (
    cd "$1" || exit 1
    {
      printf '%s\n' "$2"
      find . -type f ! -name MANIFEST.sha256 ! -name MANIFEST.sha256.tmp | LC_ALL=C sort | while IFS= read -r f; do
        h="$(_litert_sha256 "$f")" || exit 1
        [ -n "$h" ] || exit 1
        printf '%s  %s\n' "$h" "$f"
      done
    } > MANIFEST.sha256.tmp || { rm -f MANIFEST.sha256.tmp; exit 1; }
    mv MANIFEST.sha256.tmp MANIFEST.sha256
  )
}

# litert_package <platform> <src> <built-library> <dest> <commit> [bazel flags]
# Assemble a package in <dest> from a LiteRT-LM checkout and the library Bazel built from it, and
# record the flags it was built with (by default, this host's for <platform>). Runs where the build
# ran: on the Mac, on the Jetson, or inside the build container.
litert_package() {
  local platform="$1" src="$2" built="$3" dest="$4" commit="$5" flags="${6:-}" cfg ext f h n=0
  litert_platform_ok "$platform" || { echo "unknown platform: $platform" >&2; return 2; }
  cfg="$(_litert_config "$platform")"; ext="$(_litert_ext "$platform")"
  [ -f "$built" ] || { echo "no built library at $built" >&2; return 1; }
  mkdir -p "$dest/include" || return 1
  cp -L "$built" "$dest/liblitert-lm.$ext" || return 1
  for f in "$src/prebuilt/$cfg"/*."$ext"; do
    [ -f "$f" ] || continue
    if _litert_is_lfs_pointer "$f"; then
      echo "prebuilt/$cfg/${f##*/} is a Git LFS pointer, not a library; run: git -C $src lfs pull" >&2
      return 1
    fi
    cp -L "$f" "$dest/" || return 1
    n=$((n + 1))
  done
  [ "$n" -gt 0 ] || { echo "no prebuilt libraries in $src/prebuilt/$cfg" >&2; return 1; }
  chmod u+w "$dest"/*."$ext" || return 1
  case "$platform" in
    macos-arm64) _litert_fix_macho "$dest" || return 1 ;;
    linux-arm64) _litert_fix_elf "$dest" || return 1 ;;
  esac
  for h in $LITERT_HEADERS; do
    cp "$src/c/$h" "$dest/include/$h" || return 1
  done
  cp "$src/LICENSE" "$dest/LICENSE" || return 1
  litert_write_manifest "$dest" "$(litert_manifest_header "$platform" "$commit" "$flags")"
}

# Move a finished package into place. A process already running from the old copy keeps its files.
_litert_install() { # <stage> <dest>
  local stage="$1" dest="$2" old=""
  mkdir -p "$(dirname "$dest")" || return 1
  if [ -e "$dest" ]; then
    old="$dest.old.$$"
    rm -rf "$old"
    mv "$dest" "$old" || return 1
  fi
  if ! mv "$stage" "$dest"; then
    if [ -n "$old" ]; then mv "$old" "$dest"; fi
    return 1
  fi
  if [ -n "$old" ]; then rm -rf "$old"; fi
  return 0
}

# ── verification ─────────────────────────────────────────────────────────────

_litert_problem() { LITERT_VERIFY_PROBLEMS="${LITERT_VERIFY_PROBLEMS}${LITERT_VERIFY_PROBLEMS:+$_LITERT_NL}$*"; }
_litert_note()    { LITERT_VERIFY_NOTES="${LITERT_VERIFY_NOTES}${LITERT_VERIFY_NOTES:+$_LITERT_NL}$*"; }

# litert_verify <dir> [libs]: is this a package as litert_package made it, unchanged since? [libs]
# are the libraries it must carry: LITERT_REQUIRED_LIBS unless given, LITERT_DESKTOP_REQUIRED_LIBS
# for the desktop app's copy.
#
# Sets LITERT_VERIFY_PROBLEMS and LITERT_VERIFY_NOTES (one per line; a note is a check this host
# could not make), LITERT_VERIFY_FILES (how many files matched), and LITERT_VERIFY_HEADER,
# LITERT_VERIFY_PLATFORM and LITERT_VERIFY_COMMIT from the manifest. Returns 0 when there are no
# problems. Call it directly, not in $(...), or the variables are lost.
#
# The manifest comes first and must account for every file. Only a package whose hashes hold is
# inspected further, and only then is its library loaded: nothing that failed a hash is executed.
litert_verify() {
  local dir="$1" required="${2:-$LITERT_REQUIRED_LIBS}" m line hash path got n=0 listed f lib ext
  LITERT_VERIFY_PROBLEMS=""; LITERT_VERIFY_NOTES=""; LITERT_VERIFY_FILES=0
  LITERT_VERIFY_HEADER=""; LITERT_VERIFY_PLATFORM=""; LITERT_VERIFY_COMMIT=""
  if [ ! -d "$dir" ]; then _litert_problem "no such directory: $dir"; return 1; fi
  m="$dir/MANIFEST.sha256"
  if [ ! -f "$m" ]; then _litert_problem "no MANIFEST.sha256 in $dir"; return 1; fi
  LITERT_VERIFY_HEADER="$(sed -n 1p "$m")"
  case "$LITERT_VERIFY_HEADER" in
    "litert-lm C API "*) ;;
    *) _litert_problem "MANIFEST.sha256 does not start with the line that describes the build"; return 1 ;;
  esac
  LITERT_VERIFY_PLATFORM="$(_litert_header_platform "$LITERT_VERIFY_HEADER")"
  LITERT_VERIFY_COMMIT="$(_litert_header_commit "$LITERT_VERIFY_HEADER")"
  if ! litert_platform_ok "$LITERT_VERIFY_PLATFORM"; then
    _litert_problem "the manifest names no known platform: $LITERT_VERIFY_HEADER"; return 1
  fi
  ext="$(_litert_ext "$LITERT_VERIFY_PLATFORM")"

  listed=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    hash="${line%%  *}"; path="${line#*  }"
    case "$hash" in *[!0-9a-f]*|"") _litert_problem "malformed manifest line: $line"; continue ;; esac
    if [ "${#hash}" -ne 64 ] || [ "$path" = "$line" ]; then _litert_problem "malformed manifest line: $line"; continue; fi
    case "$path" in ./*) ;; *) _litert_problem "manifest path not under the package: $path"; continue ;; esac
    case "/$path/" in */../*) _litert_problem "manifest path leaves the package: $path"; continue ;; esac
    listed="$listed$_LITERT_NL$path"
    if [ -L "$dir/$path" ]; then _litert_problem "is a symlink: $path"; continue; fi
    if [ ! -f "$dir/$path" ]; then _litert_problem "missing: $path"; continue; fi
    got="$(_litert_sha256 "$dir/$path")"
    if [ "$got" != "$hash" ]; then _litert_problem "changed since it was packaged: $path"; continue; fi
    n=$((n + 1))
  done <<EOF
$(sed -n '2,$p' "$m")
EOF
  LITERT_VERIFY_FILES=$n
  [ "$n" -gt 0 ] || _litert_problem "the manifest lists no files"

  while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ "$f" = ./MANIFEST.sha256 ] && continue
    case "$listed$_LITERT_NL" in
      *"$_LITERT_NL$f$_LITERT_NL"*) ;;
      *) _litert_problem "not in the manifest: $f" ;;
    esac
  done <<EOF
$(cd "$dir" && find . \( -type f -o -type l \) 2>/dev/null | LC_ALL=C sort)
EOF

  for lib in $required; do
    case "$listed$_LITERT_NL" in
      *"$_LITERT_NL./$lib.$ext$_LITERT_NL"*) ;;
      *) _litert_problem "the package has no $lib.$ext" ;;
    esac
  done
  case "$listed$_LITERT_NL" in
    *"$_LITERT_NL./LICENSE$_LITERT_NL"*) ;;
    *) _litert_problem "the package has no LICENSE" ;;
  esac
  [ -z "$LITERT_VERIFY_PROBLEMS" ] || return 1

  case "$LITERT_VERIFY_PLATFORM" in
    macos-arm64) _litert_verify_macho "$dir" ;;
    linux-arm64) _litert_verify_elf "$dir" ;;
  esac
  [ -z "$LITERT_VERIFY_PROBLEMS" ] || return 1
  _litert_load_check "$dir"
  [ -z "$LITERT_VERIFY_PROBLEMS" ]
}

_litert_verify_macho() {
  local dir="$1" f name id dep rp rpaths archs needs_rpath
  if ! command -v otool >/dev/null 2>&1; then
    _litert_note "Mach-O checks skipped: no otool on this host"; return 0
  fi
  for f in "$dir"/*.dylib; do
    [ -f "$f" ] || continue
    name="${f##*/}"
    if command -v lipo >/dev/null 2>&1; then
      archs="$(lipo -archs "$f" 2>/dev/null)"
      case " $archs " in
        *" arm64 "*) ;;
        *) _litert_problem "$name is not an arm64 Mach-O (${archs:-unreadable})"; continue ;;
      esac
    fi
    id="$(otool -D "$f" 2>/dev/null | sed -n 2p)"
    if [ "$name" = liblitert-lm.dylib ] && [ "$id" != "@rpath/liblitert-lm.dylib" ]; then
      _litert_problem "liblitert-lm.dylib is named ${id:-nothing}, not @rpath/liblitert-lm.dylib"
    fi
    needs_rpath=false
    while IFS= read -r dep; do
      [ -n "$dep" ] || continue
      [ "$dep" = "$id" ] && continue
      case "$dep" in
        /usr/lib/*|/System/Library/*) ;;
        # The Swift runtime macOS ships in /usr/lib/swift, which the prebuilts search.
        @rpath/libswift*) ;;
        @rpath/*|@loader_path/*)
          case "$dep" in @rpath/*) needs_rpath=true ;; esac
          [ -f "$dir/${dep##*/}" ] || _litert_problem "$name needs $dep, which is not in the package" ;;
        *) _litert_problem "$name needs $dep, which is neither in the package nor part of macOS" ;;
      esac
    done <<EOF
$(otool -L "$f" 2>/dev/null | sed -n '2,$p' | sed 's/^[[:space:]]*//; s/ (compatibility version.*$//')
EOF
    rpaths="$(_litert_rpaths "$f")"
    while IFS= read -r rp; do
      [ -n "$rp" ] || continue
      case "$rp" in
        @loader_path|/usr/lib/swift) ;;
        *) _litert_problem "$name searches $rp for libraries" ;;
      esac
    done <<EOF
$rpaths
EOF
    if [ "$needs_rpath" = true ]; then
      case "$_LITERT_NL$rpaths$_LITERT_NL" in
        *"${_LITERT_NL}@loader_path${_LITERT_NL}"*) ;;
        *) _litert_problem "$name loads @rpath libraries but does not search @loader_path" ;;
      esac
    fi
    if command -v codesign >/dev/null 2>&1; then
      codesign -v "$f" >/dev/null 2>&1 || _litert_problem "$name has no valid code signature"
    else
      _litert_note "signature check skipped: no codesign on this host"
    fi
  done
}

_litert_verify_elf() {
  local dir="$1" f name dyn kind val runpath rpath needed lib skip_note=""
  command -v file >/dev/null 2>&1 || _litert_note "architecture check skipped: no file(1) on this host"
  for f in "$dir"/*.so; do
    [ -f "$f" ] || continue
    name="${f##*/}"
    if command -v file >/dev/null 2>&1; then
      case "$(file -b "$f" 2>/dev/null)" in
        *"ELF 64-bit"*aarch64*) ;;
        *) _litert_problem "$name is not a 64-bit ARM (aarch64) ELF library"; continue ;;
      esac
    fi
    if ! dyn="$(_litert_elf_dynamic "$f")"; then skip_note="ELF dynamic-section checks skipped: no readelf or objdump on this host"; break; fi
    runpath=""; rpath=""; needed=""
    while read -r kind val; do
      case "$kind" in
        NEEDED)  needed="$needed $val" ;;
        RUNPATH) runpath="$val" ;;
        RPATH)   rpath="$val" ;;
      esac
    done <<EOF
$dyn
EOF
    # shellcheck disable=SC2016
    [ "$runpath" = '$ORIGIN' ] || _litert_problem "$name searches '${runpath:-nothing}', not \$ORIGIN"
    # shellcheck disable=SC2016
    if [ -n "$rpath" ] && [ "$rpath" != '$ORIGIN' ]; then _litert_problem "$name has an RPATH of '$rpath'"; fi
    for lib in $needed; do
      [ -f "$dir/$lib" ] && continue
      case " $LITERT_LINUX_SYSTEM_LIBS " in
        *" $lib "*) ;;
        *) _litert_note "$name needs $lib from the system; ldd on the device is what can confirm it" ;;
      esac
    done
  done
  [ -z "$skip_note" ] || _litert_note "$skip_note"
}

# dlopen the library the way the backend does (absolute path, RTLD_NOW | RTLD_LOCAL, from another
# directory, no library-path variables) and find a C API symbol in it. Only where the host can.
_litert_load_check() {
  local dir="$1" ext lib tmp out libs=""
  case "$(uname -s)/$(uname -m)/$LITERT_VERIFY_PLATFORM" in
    Darwin/arm64/macos-arm64) ;;
    Linux/aarch64/linux-arm64|Linux/arm64/linux-arm64) libs="-ldl" ;;
    *) _litert_note "load check skipped: this host cannot load a $LITERT_VERIFY_PLATFORM library"; return 0 ;;
  esac
  if ! command -v cc >/dev/null 2>&1; then
    _litert_note "load check skipped: no C compiler (cc) on this host"; return 0
  fi
  ext="$(_litert_ext "$LITERT_VERIFY_PLATFORM")"
  lib="$(cd "$dir" && pwd)/liblitert-lm.$ext"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/litert-probe.XXXXXX")" || { _litert_note "load check skipped: no temporary directory"; return 0; }
  printf '%s\n' \
    '#include <dlfcn.h>' '#include <stdio.h>' \
    'int main(int argc, char **argv) {' \
    '  void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);' \
    '  if (!h) { fprintf(stderr, "%s\n", dlerror()); return 1; }' \
    '  if (!dlsym(h, argv[2])) { fprintf(stderr, "%s\n", dlerror()); return 2; }' \
    '  return 0;' \
    '}' > "$tmp/probe.c"
  # shellcheck disable=SC2086
  if ! cc -o "$tmp/probe" "$tmp/probe.c" $libs >/dev/null 2>&1; then
    rm -rf "$tmp"; _litert_note "load check skipped: cc could not build the probe"; return 0
  fi
  if ! out="$(cd / && env -u DYLD_LIBRARY_PATH -u DYLD_FALLBACK_LIBRARY_PATH -u LD_LIBRARY_PATH \
                 "$tmp/probe" "$lib" litert_lm_engine_create 2>&1)"; then
    _litert_problem "liblitert-lm.$ext does not load: $out"
  fi
  rm -rf "$tmp"
  return 0
}

# ── state, for the banner, doctor and status ─────────────────────────────────

# _litert_dir_state <dir>: LITERT_STATE is missing, present (built from the pin) or other-pin;
# LITERT_STATE_TAG is "<capi>-<commit8>" from the package's manifest. Reads one line, hashes nothing.
_litert_dir_state() {
  local h c
  LITERT_STATE=missing; LITERT_STATE_TAG=""
  [ -f "$1/MANIFEST.sha256" ] || return 0
  h="$(sed -n 1p "$1/MANIFEST.sha256")"
  c="$(_litert_header_commit "$h")"
  LITERT_STATE_TAG="$(_litert_header_capi "$h")-$(printf '%s' "$c" | cut -c1-8)"
  if [ "$c" = "$LITERT_COMMIT" ]; then LITERT_STATE=present; else LITERT_STATE=other-pin; fi
}

# litert_state <platform>: as _litert_dir_state for the recorded package, plus LITERT_STATE=none
# when nothing is recorded and LITERT_STATE_DIR for the recorded directory.
litert_state() {
  LITERT_STATE=none; LITERT_STATE_TAG=""
  LITERT_STATE_DIR="$(litert_recorded_dir "$1")"
  [ -n "$LITERT_STATE_DIR" ] || return 0
  _litert_dir_state "$LITERT_STATE_DIR"
}

# ── the desktop app ──────────────────────────────────────────────────────────

# litert_stage_desktop <dest>: put the recorded macos-arm64 package's libraries, except those
# LITERT_DESKTOP_LEAVES_OUT names, and LICENSE in <dest> (replaced), with a manifest of exactly
# those files under the package's own first line. Returns 0 when staged, 3 when no package is
# recorded, 1 when the recorded one is missing or fails verification (LITERT_VERIFY_PROBLEMS says
# why).
litert_stage_desktop() {
  local dest="$1" dir f name
  dir="$(litert_recorded_dir macos-arm64)"
  [ -n "$dir" ] || return 3
  litert_verify "$dir" || return 1
  rm -rf "$dest"
  mkdir -p "$dest" || return 1
  for f in "$dir"/*.dylib "$dir/LICENSE"; do
    name="${f##*/}"
    case " $LITERT_DESKTOP_LEAVES_OUT " in *" ${name%.dylib} "*) continue ;; esac
    cp "$f" "$dest/" || return 1
  done
  litert_write_manifest "$dest" "$LITERT_VERIFY_HEADER"
}

# ── building (talks to a person; giap.sh supplies ok/bad/info/note/run) ──────

_litert_docker_ok() {
  local v
  v="$(_litert_timeout 20 docker info --format '{{.ServerVersion}}' 2>/dev/null)" || return 1
  [ -n "$v" ]
}

# Everything a build of <platform> needs on this host. Says what is missing and how to get it.
litert_check_prereqs() {
  local p="$1" bad_count=0 t
  if ! command -v git >/dev/null 2>&1; then bad "git is not installed"; bad_count=$((bad_count + 1))
  elif ! git lfs version >/dev/null 2>&1; then
    bad "git-lfs is not installed (LiteRT's prebuilt libraries are Git LFS objects)"
    note "fix: brew install git-lfs   (apt-get install git-lfs on Linux)"
    bad_count=$((bad_count + 1))
  fi
  if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    bad "neither sha256sum nor shasum is installed"; bad_count=$((bad_count + 1))
  fi
  case "$p" in
    macos-arm64)
      case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) ;;
        *) bad "the macos-arm64 library builds on an Apple Silicon Mac, not on $(uname -s)/$(uname -m)"
           bad_count=$((bad_count + 1)) ;;
      esac
      if ! command -v bazelisk >/dev/null 2>&1; then
        bad "bazelisk is not installed (it runs the Bazel version LiteRT-LM's .bazelversion names)"
        note "fix: brew install bazelisk"
        bad_count=$((bad_count + 1))
      fi
      for t in install_name_tool codesign otool; do
        if ! command -v "$t" >/dev/null 2>&1; then
          bad "$t is not installed"; note "fix: xcode-select --install"; bad_count=$((bad_count + 1))
        fi
      done ;;
    linux-arm64)
      if [ "$(litert_build_mode linux-arm64)" = native ]; then
        if ! command -v bazelisk >/dev/null 2>&1 && ! command -v bazel >/dev/null 2>&1; then
          bad "neither bazelisk nor bazel is installed"
          note "fix: put bazelisk-linux-arm64 from github.com/bazelbuild/bazelisk/releases on PATH as bazelisk"
          bad_count=$((bad_count + 1))
        fi
        for t in clang patchelf readelf; do
          if ! command -v "$t" >/dev/null 2>&1; then
            bad "$t is not installed"; note "fix: sudo apt-get install clang patchelf binutils"
            bad_count=$((bad_count + 1))
          fi
        done
      elif ! command -v docker >/dev/null 2>&1; then
        bad "docker is not installed (linux-arm64 builds in a $LITERT_LINUX_IMAGE container)"
        bad_count=$((bad_count + 1))
      elif ! _litert_docker_ok; then
        bad "docker is installed but its daemon does not answer"
        note "fix: restart Docker Desktop, then check that 'docker info' reports a server version"
        note "or build on the Jetson itself: bash scripts/giap.sh litert build linux-arm64 there"
        bad_count=$((bad_count + 1))
      fi ;;
  esac
  [ "$bad_count" -eq 0 ]
}

# Get the source to the pinned commit with the platform's LFS objects. A clone named by
# LITERT_LM_SRC is never moved: it has to be at the pin already. The managed one is moved to it.
litert_prepare_source() {
  local platform="$1" src head cfg fresh=false
  src="$(litert_src_dir)"; cfg="$(_litert_config "$platform")"
  if [ -n "${LITERT_LM_SRC:-}" ]; then
    if ! git -C "$src" rev-parse --git-dir >/dev/null 2>&1; then
      bad "LITERT_LM_SRC=$src is not a git checkout"; return 1
    fi
    head="$(git -C "$src" rev-parse HEAD 2>/dev/null)"
    if [ "$head" != "$LITERT_COMMIT" ]; then
      bad "LITERT_LM_SRC is at ${head:-no commit}; the pin is $LITERT_COMMIT"
      note "fix: git -C \"$src\" fetch origin && git -C \"$src\" checkout --detach $LITERT_COMMIT"
      note "or unset LITERT_LM_SRC to build from $(litert_home)/src, which this tool keeps"
      return 1
    fi
  else
    if ! git -C "$src" rev-parse --git-dir >/dev/null 2>&1; then
      if [ -e "$src" ] && [ -n "$(ls -A "$src" 2>/dev/null)" ]; then
        bad "$src exists and is not a git checkout; move it aside"; return 1
      fi
      info "cloning LiteRT-LM into $src (LFS objects follow, for prebuilt/$cfg only)"
      run env GIT_LFS_SKIP_SMUDGE=1 git clone --filter=blob:none "$LITERT_REPO_URL" "$src" || return 1
      fresh=true
    fi
    head="$(git -C "$src" rev-parse HEAD 2>/dev/null)"
    if [ "$head" != "$LITERT_COMMIT" ]; then
      if [ -n "$(git -C "$src" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
        bad "$src has local changes; not moving it to the pin"; return 1
      fi
      if [ "$fresh" != true ]; then run git -C "$src" fetch origin || return 1; fi
      if [ "${DRY_RUN:-false}" != true ] && ! git -C "$src" cat-file -e "$LITERT_COMMIT^{commit}" 2>/dev/null; then
        # By URL, not origin: a clone made from upstream before the fork existed has no giap-main.
        if ! run git -C "$src" fetch "$LITERT_REPO_URL" "$LITERT_COMMIT"; then
          note "$LITERT_REPO_URL does not have the pin; push giap-main there first"
          note "or: LITERT_LM_SRC=<a giap-main clone at $LITERT_COMMIT> bash scripts/giap.sh litert build $platform"
          return 1
        fi
      fi
      run env GIT_LFS_SKIP_SMUDGE=1 git -C "$src" -c advice.detachedHead=false checkout --detach "$LITERT_COMMIT" || return 1
    fi
  fi
  run git -C "$src" lfs pull --include="prebuilt/$cfg/*" || return 1
}

# The flags beyond litert_bazel_flags: UI, and the optional distdir and job cap.
_litert_bazel_extra() { # <distdir as Bazel will see it>
  printf -- '--curses=no --color=no'
  if [ -n "$1" ]; then printf -- ' --distdir=%s' "$1"; fi
  if [ -n "${LITERT_BAZEL_JOBS:-}" ]; then printf -- ' --jobs=%s' "$LITERT_BAZEL_JOBS"; fi
}

# A build with this host's own Bazel: macos-arm64 on a Mac, linux-arm64 on the Jetson.
_litert_build_native() { # <platform> <src> <stage>
  local platform="$1" src="$2" stage="$3" flags bazel=bazelisk built
  flags="$(litert_bazel_flags "$platform" native)"
  if [ "$platform" = linux-arm64 ] && ! command -v bazelisk >/dev/null 2>&1; then bazel=bazel; fi
  # shellcheck disable=SC2086
  set -- build $flags --curses=no --color=no
  if [ -n "${LITERT_DISTDIR:-}" ]; then set -- "$@" "--distdir=$LITERT_DISTDIR"; fi
  if [ -n "${LITERT_BAZEL_JOBS:-}" ]; then set -- "$@" "--jobs=$LITERT_BAZEL_JOBS"; fi
  set -- "$@" "$LITERT_TARGET"
  info "in $src:"
  if [ "${DRY_RUN:-false}" = true ]; then run "$bazel" "$@"; return 0; fi
  ( cd "$src" && run "$bazel" "$@" ) || return 1
  built="$src/bazel-bin/c/liblitert-lm.$(_litert_ext "$platform")"
  if [ ! -f "$built" ]; then
    bad "Bazel finished, but $built is not there"
    note "it built: $(ls "$src/bazel-bin/c" 2>/dev/null | tr '\n' ' ')"
    return 1
  fi
  litert_package "$platform" "$src" "$built" "$stage" "$LITERT_COMMIT" "$flags"
}

# The script the linux-arm64 container runs. Values from this side are written in; \$ is the
# container's own. It builds, then packages and verifies with this same file, mounted at /giap.
_litert_container_script() {
  local build_flags flags
  build_flags="$(litert_bazel_flags linux-arm64 docker)"
  flags="$build_flags $(_litert_bazel_extra "${LITERT_DISTDIR:+/distdir}") --symlink_prefix=/"
  cat <<EOF
set -eu
export DEBIAN_FRONTEND=noninteractive
echo '==> toolchain: clang-$LITERT_LINUX_CLANG, git-lfs, patchelf (apt)'
apt-get update -qq
apt-get install -y -qq --no-install-recommends ca-certificates curl git git-lfs python3 patch unzip zip \\
  file binutils build-essential patchelf clang-$LITERT_LINUX_CLANG >/dev/null
ln -sf /usr/bin/clang-$LITERT_LINUX_CLANG /usr/local/bin/clang
ln -sf /usr/bin/clang++-$LITERT_LINUX_CLANG /usr/local/bin/clang++
git config --global --add safe.directory '*'
B=/cache/bin/bazelisk-$LITERT_BAZELISK_VERSION
if ! printf '%s  %s\\n' $LITERT_BAZELISK_LINUX_ARM64_SHA256 "\$B" | sha256sum -c --status - 2>/dev/null; then
  echo '==> bazelisk $LITERT_BAZELISK_VERSION (linux-arm64, checked against its published SHA-256)'
  mkdir -p /cache/bin
  curl -fsSL --proto '=https' --tlsv1.2 -o "\$B.part" \\
    https://github.com/bazelbuild/bazelisk/releases/download/$LITERT_BAZELISK_VERSION/bazelisk-linux-arm64
  if ! printf '%s  %s\\n' $LITERT_BAZELISK_LINUX_ARM64_SHA256 "\$B.part" | sha256sum -c --status -; then
    rm -f "\$B.part"; echo 'bazelisk download does not match its SHA-256; nothing was run' >&2; exit 1
  fi
  chmod +x "\$B.part"; mv "\$B.part" "\$B"
fi
export BAZELISK_HOME=/cache/bazelisk
cd /src
echo '==> bazel build $LITERT_TARGET (linux-arm64)'
"\$B" --output_user_root=/cache/bazel build $flags $LITERT_TARGET
bin="\$("\$B" --output_user_root=/cache/bazel info $flags bazel-bin)"
if [ ! -f "\$bin/c/liblitert-lm.so" ]; then
  echo "Bazel finished, but \$bin/c has no liblitert-lm.so. It has:" >&2; ls "\$bin/c" >&2; exit 1
fi
echo '==> package and verify'
. /giap/litert-setup.sh
litert_package linux-arm64 /src "\$bin/c/liblitert-lm.so" /out $LITERT_COMMIT '$build_flags'
if ! litert_verify /out; then printf 'verify: %s\\n' "\$LITERT_VERIFY_PROBLEMS" >&2; exit 1; fi
[ -z "\$LITERT_VERIFY_NOTES" ] || printf 'note: %s\\n' "\$LITERT_VERIFY_NOTES"
echo "==> \$LITERT_VERIFY_FILES files verified in the container"
chown -R "\$HOST_UID:\$HOST_GID" /out 2>/dev/null || true
EOF
}

_litert_build_linux() { # <src> <stage>
  local src="$1" stage="$2"
  set -- run --rm --platform linux/arm64 \
    -v "$src:/src:ro" -v "$stage:/out" -v "$LITERT_LIB_DIR:/giap:ro" -v "$LITERT_DOCKER_VOLUME:/cache"
  if [ -n "${LITERT_DISTDIR:-}" ]; then set -- "$@" -v "$LITERT_DISTDIR:/distdir:ro"; fi
  set -- "$@" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" "$LITERT_LINUX_IMAGE" bash -c "$(_litert_container_script)"
  run docker "$@"
}

# litert_build <platform>: source, build, package, verify, then move into place and record.
# Under DRY_RUN=true it prints the commands and changes nothing.
litert_build() {
  local platform="$1" src stage dest dry="${DRY_RUN:-false}"
  if ! litert_platform_ok "$platform"; then
    bad "unknown platform '$platform': macos-arm64 or linux-arm64"; return 2
  fi
  if ! litert_check_prereqs "$platform"; then
    if [ "$dry" = true ]; then warn "dry run: showing the commands anyway"; else return 1; fi
  fi
  if [ -n "${LITERT_DISTDIR:-}" ]; then
    if [ ! -d "$LITERT_DISTDIR" ]; then bad "--distdir $LITERT_DISTDIR is not a directory"; return 1; fi
    LITERT_DISTDIR="$(cd "$LITERT_DISTDIR" && pwd)"
    info "Bazel takes archives from $LITERT_DISTDIR before it downloads them"
  fi
  litert_prepare_source "$platform" || return 1
  src="$(litert_src_dir)"
  if [ -d "$src" ]; then src="$(cd "$src" && pwd)"; fi
  dest="$(litert_package_dir "$platform")"
  if [ "$dry" = true ]; then
    stage="$(litert_home)/.stage-$platform.XXXXXX"
    case "$(litert_build_mode "$platform")" in
      native) _litert_build_native "$platform" "$src" "$stage" ;;
      docker) _litert_build_linux "$src" "$stage" ;;
    esac
    info "then: package (the library, prebuilt/$(_litert_config "$platform")/*.$(_litert_ext "$platform"), rpath fixes, headers, LICENSE, MANIFEST.sha256),"
    info "verify it, move it to $dest and record it in $(litert_record_file "$platform")"
    return 0
  fi
  mkdir -p "$(litert_home)" || return 1
  rm -rf "$(litert_home)/.stage-$platform".*
  stage="$(mktemp -d "$(litert_home)/.stage-$platform.XXXXXX")" || return 1
  # mktemp makes it 0700; the package is read by whatever runs the pond, and rsync keeps the mode.
  chmod 755 "$stage" || { rm -rf "$stage"; return 1; }
  case "$(litert_build_mode "$platform")" in
    native) _litert_build_native "$platform" "$src" "$stage" ;;
    docker) _litert_build_linux "$src" "$stage" ;;
  esac || { rm -rf "$stage"; bad "the $platform build did not finish; nothing was installed"; return 1; }
  if ! litert_verify "$stage"; then
    bad "the new $platform package fails verification; nothing was installed"
    printf '%s\n' "$LITERT_VERIFY_PROBLEMS" | while IFS= read -r line; do note "$line"; done
    rm -rf "$stage"
    return 1
  fi
  _litert_install "$stage" "$dest" || { rm -rf "$stage"; bad "could not move the package to $dest"; return 1; }
  litert_record "$platform" "$dest" || { bad "could not write $(litert_record_file "$platform")"; return 1; }
  ok "LiteRT-LM C API $LITERT_CAPI_VERSION for $platform: $LITERT_VERIFY_FILES files verified in $dest"
  note "recorded in $(litert_record_file "$platform")"
  case "$platform" in
    macos-arm64)
      note "the desktop app ships it: npm run stage:server copies it into pond-desktop/resources/litert-lm"
      note "a pond started from target/ finds it through the record when it starts" ;;
    linux-arm64)
      if [ "$(litert_build_mode linux-arm64)" = native ]; then
        note "a pond started on this host finds it through the record"
        note "scripts/jetson.sh deploy ships what the dev machine records: copy $dest there, then"
        note "bash scripts/giap.sh litert import <that copy>"
      else
        note "bash scripts/jetson.sh deploy copies it to the device and checks it there"
      fi ;;
  esac
  return 0
}

# litert_import <dir>: a package built on another host, the Jetson, taken in as if it had been
# built here: verified, held to the pin, copied into place and recorded, so that
# scripts/jetson.sh deploy ships it. Under DRY_RUN=true it checks and changes nothing.
litert_import() {
  local src="$1" platform dest stage
  if ! litert_verify "$src"; then
    bad "$src is not a package that verifies; nothing was imported"
    printf '%s\n' "$LITERT_VERIFY_PROBLEMS" | while IFS= read -r line; do note "$line"; done
    return 1
  fi
  platform="$LITERT_VERIFY_PLATFORM"
  if [ "$LITERT_VERIFY_COMMIT" != "$LITERT_COMMIT" ] \
     || [ "$(_litert_header_capi "$LITERT_VERIFY_HEADER")" != "$LITERT_CAPI_VERSION" ]; then
    bad "$src was built from LiteRT-LM ${LITERT_VERIFY_COMMIT:-an unknown commit}; the pin is $LITERT_COMMIT (C API $LITERT_CAPI_VERSION)"
    return 1
  fi
  dest="$(litert_package_dir "$platform")"
  if [ "${DRY_RUN:-false}" = true ]; then
    info "would copy $src to $dest and record it in $(litert_record_file "$platform")"
    return 0
  fi
  mkdir -p "$(litert_home)" || return 1
  stage="$(mktemp -d "$(litert_home)/.stage-$platform.XXXXXX")" || return 1
  if ! { chmod 755 "$stage" && cp -R "$src/." "$stage/" && litert_verify "$stage"; }; then
    rm -rf "$stage"; bad "the copy of $src does not verify; nothing was imported"; return 1
  fi
  _litert_install "$stage" "$dest" || { rm -rf "$stage"; bad "could not move the package to $dest"; return 1; }
  litert_record "$platform" "$dest" || { bad "could not write $(litert_record_file "$platform")"; return 1; }
  ok "LiteRT-LM C API $LITERT_CAPI_VERSION for $platform: $LITERT_VERIFY_FILES files imported into $dest"
  note "recorded in $(litert_record_file "$platform")"
}

# ── reporting ────────────────────────────────────────────────────────────────

# One package, verified, for status and doctor. <label> <dir>. Returns litert_verify's result.
litert_report_dir() {
  local label="$1" dir="$2" line
  if litert_verify "$dir"; then
    ok "$label: $LITERT_VERIFY_FILES files match MANIFEST.sha256 ($dir)"
  else
    bad "$label fails verification ($dir)"
    printf '%s\n' "$LITERT_VERIFY_PROBLEMS" | while IFS= read -r line; do note "$line"; done
  fi
  [ -z "$LITERT_VERIFY_HEADER" ] || note "$LITERT_VERIFY_HEADER"
  if [ -n "$LITERT_VERIFY_NOTES" ]; then
    printf '%s\n' "$LITERT_VERIFY_NOTES" | while IFS= read -r line; do note "$line"; done
  fi
  [ -z "$LITERT_VERIFY_PROBLEMS" ]
}

# Every recorded package, and on Linux the one deployed under the pond's data directory
# (<data-dir> is optional). Returns 1 when anything recorded or deployed is missing or fails.
litert_status() { # [data-dir]
  local p rc=0 deployed
  info "pin: C API $LITERT_CAPI_VERSION, LiteRT-LM $LITERT_COMMIT ($(litert_tag))"
  for p in macos-arm64 linux-arm64; do
    litert_state "$p"
    case "$LITERT_STATE" in
      none)
        info "$p: no package recorded"
        note "build: bash scripts/giap.sh litert build $p" ;;
      missing)
        bad "$p: recorded at $LITERT_STATE_DIR, which has no package any more"
        note "fix: bash scripts/giap.sh litert build $p"
        rc=1 ;;
      *)
        litert_report_dir "$p" "$LITERT_STATE_DIR" || rc=1
        if [ "$LITERT_STATE" = other-pin ]; then
          warn "$p was built from $LITERT_STATE_TAG; the pin is $(litert_tag)"
          note "fix: bash scripts/giap.sh litert build $p"
        fi ;;
    esac
  done
  if [ -n "${1:-}" ] && [ "$(uname -s)" = Linux ]; then
    deployed="$1/$(litert_device_subdir)"
    if [ -d "$deployed" ]; then litert_report_dir "deployed" "$deployed" || rc=1
    else info "deployed: none at $deployed"; fi
  fi
  return $rc
}
