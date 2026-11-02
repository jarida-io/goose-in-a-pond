#!/usr/bin/env bash
# Tests for litert-setup.sh. Run: bash scripts/lib/litert-setup.test.sh   (also under /bin/bash 3.2)
#
# No network, no Bazel, no Docker. Each test gets a scratch directory with its own HOME and a PATH of
# symlinks to only the tools the code needs, plus fake install_name_tool, codesign, otool, lipo,
# patchelf, readelf, file, bazelisk, docker and git-lfs that keep the state of every stand-in library
# (install name, rpaths, signature, RUNPATH, NEEDED) in fixture files and log what they are asked.
# So the packaging, manifest and verification logic runs for real on any host.
#
# Two native tests use the real tools instead, where the host has them: real Mach-O dylibs on an
# Apple Silicon Mac (rewritten, signed, verified and loaded) and real ELF libraries on Linux
# (rewritten by patchelf, read back by readelf, loaded through $ORIGIN). The one for the other
# platform is skipped; one for this platform that lacks a tool is skipped too, unless
# LITERT_TEST_REQUIRE_NATIVE=1 (set in CI), which makes that a failure.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# Through pwd, so a TMPDIR ending in / does not leave // in paths the code itself normalises.
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/litert-setup-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"
PASSED=0; FAILED=0; SKIPPED=0
SKIP=77          # a native test this host could run but cannot: a failure when native tests are required
NOT_HERE=78      # a native test for another platform: always a skip
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  elif [ "$rc" -eq "$NOT_HERE" ] || { [ "$rc" -eq "$SKIP" ] && [ "${LITERT_TEST_REQUIRE_NATIVE:-0}" != 1 ]; }; then
    SKIPPED=$((SKIPPED + 1)); printf 'SKIP: %s (%s)\n' "$name" "$(printf '%s' "$out" | tail -1)"
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
ZEROS=0000000000000000000000000000000000000000000000000000000000000000
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }
# names <dir>: the entries of <dir>, dot files included, one per line -- what `ls -A` lists, but
# from a glob, so a file name reaches grep intact.
names()    { local f; for f in "$1"/* "$1"/.[!.]* "$1"/..?*; do { [ -e "$f" ] || [ -L "$f" ]; } && printf '%s\n' "${f##*/}"; done; }
skip()     { echo "$*"; exit "$SKIP"; }
not_here() { echo "$*"; exit "$NOT_HERE"; }

# Real tools the library uses, and nothing else. No cc, so the load check is skipped unless a test
# brings one; no otool, codesign, patchelf or readelf, which are faked per test.
TOOLS="$SCRATCH/tools"; mkdir -p "$TOOLS"
for tool in bash sh env cat cp mv rm mkdir ln ls chmod find sort sed awk grep head tail tr cut wc date \
            dirname basename mktemp uname id sha256sum shasum perl timeout git touch dd od cmp stat; do
  p="$(command -v "$tool" 2>/dev/null)" && [ -n "$p" ] && [ -x "$p" ] && ln -sf "$p" "$TOOLS/$tool"
done

# A fresh world per test: its own HOME, a PATH of fakes then the tools above, the library sourced.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin" "$W/fx"
  export HOME="$W/home" PATH="$W/bin:$TOOLS" GIAP_LITERT_HOME="$W/home/.giap/litert-lm"
  export W FX="$W/fx" TOOL_LOG="$W/tools.log"
  : > "$TOOL_LOG"
  unset LITERT_LM_SRC LITERT_DISTDIR LITERT_BAZEL_JOBS DRY_RUN FAKE_BAZEL_FAIL FAKE_DOCKER_DOWN
  export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid
  # shellcheck source=litert-setup.sh
  source "$HERE/litert-setup.sh"
}
tools_log() { cat "$TOOL_LOG"; }

# ── fake tools ───────────────────────────────────────────────────────────────
# Per-library state lives in $FX/<basename>.<property>, so a copy of a library shares its state.
fake() { printf '#!/bin/sh\n%s\n' "$2" > "$W/bin/$1"; chmod +x "$W/bin/$1"; }
fakes() {
  fake otool '
f="$2"; n="${f##*/}"; [ -f "$f" ] || exit 1
case "$1" in
  -D) printf "%s:\n" "$f"; [ -f "$FX/$n.id" ] && cat "$FX/$n.id" ;;
  -L) printf "%s:\n" "$f"
      [ -f "$FX/$n.id" ] && printf "\t%s (compatibility version 0.0.0, current version 0.0.0)\n" "$(cat "$FX/$n.id")"
      [ -f "$FX/$n.deps" ] && while IFS= read -r d; do printf "\t%s (compatibility version 1.0.0, current version 1.0.0)\n" "$d"; done < "$FX/$n.deps" ;;
  -l) printf "%s:\nLoad command 0\n      cmd LC_SEGMENT_64\n  cmdsize 72\n  segname __TEXT\n" "$f"; i=0
      [ -f "$FX/$n.rpaths" ] && while IFS= read -r r; do i=$((i+1)); printf "Load command %s\n          cmd LC_RPATH\n      cmdsize 32\n         path %s (offset 12)\n" "$i" "$r"; done < "$FX/$n.rpaths" ;;
esac
exit 0'
  fake install_name_tool '
echo "install_name_tool $*" >> "$TOOL_LOG"
for f; do :; done; n="${f##*/}"; touch "$FX/$n.rpaths"
case "$1" in
  -id) printf "%s\n" "$2" > "$FX/$n.id" ;;
  -delete_rpath) grep -qxF -- "$2" "$FX/$n.rpaths" || { echo "no LC_RPATH load command with path: $2" >&2; exit 1; }
                 grep -vxF -- "$2" "$FX/$n.rpaths" > "$FX/$n.tmp"; mv "$FX/$n.tmp" "$FX/$n.rpaths" ;;
  -add_rpath) if grep -qxF -- "$2" "$FX/$n.rpaths"; then echo "would duplicate path, file already has LC_RPATH for: $2" >&2; exit 1; fi
              printf "%s\n" "$2" >> "$FX/$n.rpaths" ;;
esac
echo stale > "$FX/$n.sig"'
  fake codesign '
echo "codesign $*" >> "$TOOL_LOG"
for f; do :; done; n="${f##*/}"
case "$1" in
  --force) echo adhoc > "$FX/$n.sig" ;;
  -v) case "$(cat "$FX/$n.sig" 2>/dev/null)" in adhoc|google) exit 0 ;; *) echo "invalid signature" >&2; exit 1 ;; esac ;;
esac'
  fake lipo 'n="${2##*/}"; cat "$FX/$n.arch" 2>/dev/null || echo arm64'
  fake patchelf '
echo "patchelf $*" >> "$TOOL_LOG"
for f; do :; done; n="${f##*/}"
[ "$1" = --set-rpath ] && printf "%s\n" "$2" > "$FX/$n.runpath"
exit 0'
  fake readelf '
for f; do :; done; n="${f##*/}"; [ -f "$f" ] || exit 1
printf "\nDynamic section at offset 0x1000 contains 4 entries:\n  Tag        Type                         Name/Value\n"
[ -f "$FX/$n.needed" ] && while IFS= read -r d; do printf " 0x0000000000000001 (NEEDED)             Shared library: [%s]\n" "$d"; done < "$FX/$n.needed"
[ -f "$FX/$n.runpath" ] && printf " 0x000000000000001d (RUNPATH)            Library runpath: [%s]\n" "$(cat "$FX/$n.runpath")"
exit 0'
  fake file 'for f; do :; done; n="${f##*/}"; cat "$FX/$n.file" 2>/dev/null || echo "ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked"'
  fake git-lfs '[ "$1" = version ] && { echo "git-lfs/3.7.0 (fake)"; exit 0; }; echo "git-lfs $*" >> "$TOOL_LOG"'
  fake bazelisk '
echo "bazelisk $* (in $PWD)" >> "$TOOL_LOG"
[ -n "${FAKE_BAZEL_FAIL:-}" ] && exit 1
mkdir -p bazel-bin/c && printf "built by bazel\n" > bazel-bin/c/liblitert-lm.dylib
printf "built by bazel\n" > bazel-bin/c/liblitert-lm.so'
  # docker info answers unless FAKE_DOCKER_DOWN; docker run plays the container: it packages a
  # stand-in library into the /out mount with this same library, as the real container does.
  fake docker '
case "$1" in
  info) [ -n "${FAKE_DOCKER_DOWN:-}" ] && { echo "500 Internal Server Error" >&2; exit 1; }; echo 28.4.0; exit 0 ;;
esac
out=""; src=""; giap=""; prev=""; for a; do
  if [ "$prev" = -v ]; then case "$a" in *:/out) out="${a%:/out}" ;; *:/src:ro) src="${a%:/src:ro}" ;; *:/giap:ro) giap="${a%:/giap:ro}" ;; esac; fi
  prev="$a"; last="$a"; done
printf "%s" "$*" > "$W/docker-args"; printf "%s" "$last" > "$W/container-script.sh"
[ -n "${FAKE_BAZEL_FAIL:-}" ] && exit 1
bash -c ". \"$giap/litert-setup.sh\"; litert_package linux-arm64 \"$src\" \"$src/fake-out/liblitert-lm.so\" \"$out\" $LITERT_COMMIT"'
  fake cc 'exit 1'
}
# A fake uname, for code that asks which host it is on.
fake_host() { fake uname "case \"\$1\" in -s) echo $1 ;; -m) echo $2 ;; *) echo $1 ;; esac"; }

# ── stand-in source trees, shaped like LiteRT-LM's ───────────────────────────
MAC_PREBUILTS="libGemmaModelConstraintProvider libLiteRt libLiteRtMetalAccelerator libLiteRtTopKMetalSampler libLiteRtTopKWebGpuSampler libLiteRtWebGpuAccelerator libwebgpu_dawn"
LNX_PREBUILTS="libGemmaModelConstraintProvider libLiteRt libLiteRtTopKWebGpuSampler libLiteRtWebGpuAccelerator libwebgpu_dawn"

# mk_src <dir> <platform>: headers, LICENSE, a BUILD file and the prebuilts, with the fixture state
# the real ones have (Google-signed, /usr/lib/swift or no rpath; google3 RUNPATHs on two .so files).
mk_src() {
  local d="$1" p="$2" cfg ext n
  cfg="$(_litert_config "$p")"; ext="$(_litert_ext "$p")"
  mkdir -p "$d/c" "$d/prebuilt/$cfg"
  for n in $LITERT_HEADERS; do printf '/* %s */\n' "$n" > "$d/c/$n"; done
  printf 'Apache License, Version 2.0\n' > "$d/LICENSE"
  printf 'filegroup(name = "prebuilt")\n' > "$d/prebuilt/$cfg/BUILD"
  if [ "$p" = macos-arm64 ]; then
    for n in $MAC_PREBUILTS; do
      printf 'stand-in %s\n' "$n" > "$d/prebuilt/$cfg/$n.dylib"
      printf '@rpath/%s.dylib\n' "$n" > "$FX/$n.dylib.id"; echo google > "$FX/$n.dylib.sig"
      printf '/usr/lib/libSystem.B.dylib\n' > "$FX/$n.dylib.deps"
      case "$n" in libGemmaModelConstraintProvider|libwebgpu_dawn) : > "$FX/$n.dylib.rpaths" ;;
                   *) printf '/usr/lib/swift\n' > "$FX/$n.dylib.rpaths" ;; esac
      case "$n" in *WebGpu*) printf '@rpath/libwebgpu_dawn.dylib\n' >> "$FX/$n.dylib.deps" ;; esac
    done
  else
    for n in $LNX_PREBUILTS; do
      printf 'stand-in %s\n' "$n" > "$d/prebuilt/$cfg/$n.so"
      printf 'libm.so.6\nlibc.so.6\n' > "$FX/$n.so.needed"
      case "$n" in *WebGpu*) printf 'libwebgpu_dawn.so\n' >> "$FX/$n.so.needed"
                             printf '$ORIGIN/../../_solib_arm/_U_S_Sthird_Uparty_Sdawn\n' > "$FX/$n.so.runpath" ;; esac
    done
  fi
}
# mk_built <path> <platform>: what Bazel leaves, as the real one is (sandbox install name and rpaths).
mk_built() {
  mkdir -p "$(dirname "$1")"; printf 'built by bazel\n' > "$1"
  if [ "$2" = macos-arm64 ]; then mk_built_fixtures; else
    printf 'libLiteRt.so\nlibGemmaModelConstraintProvider.so\nlibstdc++.so.6\nlibc.so.6\n' > "$FX/liblitert-lm.so.needed"
    printf '$ORIGIN:$ORIGIN/../_solib_arm/_U_A_Alitert\n' > "$FX/liblitert-lm.so.runpath"
  fi
}
mk_built_fixtures() {
  printf 'bazel-out/darwin_arm64-opt/bin/c/liblitert-lm.dylib\n' > "$FX/liblitert-lm.dylib.id"
  printf '%s\n' '@loader_path/../_solib_darwin_arm64/_U_S_Sruntime_Sgemma' '@loader_path/liblitert-lm.dylib.runfiles/litert_lm/_solib_darwin_arm64/x' '@loader_path' > "$FX/liblitert-lm.dylib.rpaths"
  printf '%s\n' /usr/lib/libc++.1.dylib @rpath/libGemmaModelConstraintProvider.dylib @rpath/libLiteRt.dylib \
    /System/Library/Frameworks/Metal.framework/Versions/A/Metal > "$FX/liblitert-lm.dylib.deps"
  echo adhoc > "$FX/liblitert-lm.dylib.sig"
}
# A packaged macos-arm64 (or linux-arm64) package at $PKG, from fakes.
mk_pkg() {
  local p="${1:-macos-arm64}"
  fakes; mk_src "$W/src" "$p"; mk_built "$W/out/liblitert-lm.$(_litert_ext "$p")" "$p"
  PKG="$W/pkg"
  litert_package "$p" "$W/src" "$W/out/liblitert-lm.$(_litert_ext "$p")" "$PKG" "$LITERT_COMMIT" >/dev/null
}
# The talking functions giap.sh supplies, replaced by a log. run honours DRY_RUN as giap.sh's does.
stubs() {
  LOG="$W/log"; : > "$LOG"
  say()  { echo "say: $*" >> "$LOG"; };  ok()   { echo "ok: $*" >> "$LOG"; }
  warn() { echo "warn: $*" >> "$LOG"; }; bad()  { echo "bad: $*" >> "$LOG"; }
  info() { echo "info: $*" >> "$LOG"; }; note() { echo "note: $*" >> "$LOG"; }
  run()  { echo "run: $*" >> "$LOG"; [ "${DRY_RUN:-false}" = true ] && return 0; "$@"; }
}
logged() { cat "$LOG"; }
# A git repository at a known commit; prints the commit.
mk_repo() {
  mkdir -p "$1" && ( cd "$1" && git init -q && printf 'x\n' > README && git add README && git commit -qm one && git rev-parse HEAD )
}

# ── the pin and the names ────────────────────────────────────────────────────

t_pin_is_a_full_commit() {
  world
  case "$LITERT_COMMIT" in *[!0-9a-f]*) echo "not hex: $LITERT_COMMIT"; return 1 ;; esac
  eq "${#LITERT_COMMIT}" 40 || return 1
  eq "$(litert_tag)" "$LITERT_CAPI_VERSION-$(printf '%s' "$LITERT_COMMIT" | cut -c1-8)"
}
t_paths() {
  world
  eq "$(litert_package_dir macos-arm64)" "$GIAP_LITERT_HOME/$(litert_tag)/macos-arm64" || return 1
  eq "$(litert_record_file linux-arm64)" "$GIAP_LITERT_HOME/.path-linux-arm64" || return 1
  eq "$(litert_device_subdir)" "lib/litert-lm/$(litert_tag)" || return 1
  eq "$(litert_src_dir)" "$GIAP_LITERT_HOME/src" || return 1
  LITERT_LM_SRC=/x/LiteRT-LM; eq "$(litert_src_dir)" /x/LiteRT-LM || return 1
  unset GIAP_LITERT_HOME; eq "$(litert_home)" "$HOME/.giap/litert-lm"
}
t_platforms() {
  world
  litert_platform_ok macos-arm64 && litert_platform_ok linux-arm64 || return 1
  ! litert_platform_ok windows-x86_64 && ! litert_platform_ok "" || return 1
  eq "$(_litert_config linux-arm64)" linux_arm64 && eq "$(_litert_ext macos-arm64)" dylib || return 1
  fake_host Darwin arm64;  eq "$(litert_default_platform)" macos-arm64 || return 1
  fake_host Linux aarch64; eq "$(litert_default_platform)" linux-arm64
}
t_bazel_flags_and_header() {
  world; fakes; fake_host Darwin arm64
  eq "$(litert_bazel_flags macos-arm64)" "-c opt --config=macos_arm64 --define=litert_runtime_link_mode=dynamic" || return 1
  h="$(litert_manifest_header linux-arm64 "$LITERT_COMMIT")"
  case "$h" in
    "litert-lm C API $LITERT_CAPI_VERSION | LiteRT-LM $LITERT_COMMIT | built "????-??-??T??:??Z" on linux-arm64 | bazel -c opt --config=linux_arm64 --define=litert_runtime_link_mode=dynamic //c:litert-lm") ;;
    *) echo "header: $h"; return 1 ;;
  esac
  eq "$(_litert_header_platform "$h")" linux-arm64 && eq "$(_litert_header_commit "$h")" "$LITERT_COMMIT" \
    && eq "$(_litert_header_capi "$h")" "$LITERT_CAPI_VERSION"
}
# The package made by hand on 2026-10-02 from upstream 3dbb23e1, which the backend was first tested
# against, still reads correctly: an older package is recognised as older, not as unreadable.
t_header_reads_the_hand_made_package() {
  world
  h='litert-lm C API 1.0.0 | LiteRT-LM 3dbb23e1e31f085a2222515282419ed470af590f | built 2026-10-02T14:54Z on macos-arm64 | bazel -c opt --config=macos_arm64 --define=litert_runtime_link_mode=dynamic //c:litert-lm'
  eq "$(_litert_header_platform "$h")" macos-arm64 \
    && eq "$(_litert_header_commit "$h")" 3dbb23e1e31f085a2222515282419ed470af590f \
    && eq "$(_litert_header_capi "$h")" 1.0.0
}

# ── packaging ────────────────────────────────────────────────────────────────

t_package_macos_layout() {
  world; mk_pkg macos-arm64 || return 1
  for n in liblitert-lm $MAC_PREBUILTS; do [ -f "$PKG/$n.dylib" ] || { echo "no $n.dylib"; return 1; }; done
  for n in $LITERT_HEADERS; do [ -f "$PKG/include/$n" ] || { echo "no include/$n"; return 1; }; done
  [ -f "$PKG/LICENSE" ] && [ -f "$PKG/MANIFEST.sha256" ] || return 1
  [ ! -e "$PKG/BUILD" ] || { echo "copied the prebuilt BUILD file"; return 1; }
  eq "$(cat "$PKG/liblitert-lm.dylib")" "built by bazel"
}
t_package_macos_rewrites_and_signs() {
  world; mk_pkg macos-arm64 || return 1
  l="$(tools_log)"
  contains "$l" "install_name_tool -id @rpath/liblitert-lm.dylib $PKG/liblitert-lm.dylib" || return 1
  contains "$l" "install_name_tool -delete_rpath @loader_path/../_solib_darwin_arm64/_U_S_Sruntime_Sgemma $PKG/liblitert-lm.dylib" || return 1
  contains "$l" "install_name_tool -delete_rpath @loader_path/liblitert-lm.dylib.runfiles/litert_lm/_solib_darwin_arm64/x $PKG/liblitert-lm.dylib" || return 1
  eq "$(cat "$FX/liblitert-lm.dylib.rpaths")" "@loader_path" || return 1
  for n in $MAC_PREBUILTS; do
    contains "$l" "install_name_tool -add_rpath @loader_path $PKG/$n.dylib" || return 1
    contains "$(cat "$FX/$n.dylib.rpaths")" "@loader_path" || return 1
  done
  eq "$(printf '%s\n' "$l" | grep -c '^codesign --force -s - ')" 8 || return 1
  for n in liblitert-lm $MAC_PREBUILTS; do eq "$(cat "$FX/$n.dylib.sig")" adhoc || { echo "$n left unsigned"; return 1; }; done
  # signing comes after every edit
  last_edit="$(printf '%s\n' "$l" | grep -n '^install_name_tool' | tail -1 | cut -d: -f1)"
  first_sign="$(printf '%s\n' "$l" | grep -n '^codesign' | head -1 | cut -d: -f1)"
  [ "$last_edit" -lt "$first_sign" ] || { echo "signed before the last edit"; return 1; }
}
t_package_leaves_an_existing_loader_path_alone() {
  world; fakes; mk_src "$W/src" macos-arm64; mk_built "$W/out/liblitert-lm.dylib" macos-arm64
  printf '/usr/lib/swift\n@loader_path\n' > "$FX/libLiteRt.dylib.rpaths"
  litert_package macos-arm64 "$W/src" "$W/out/liblitert-lm.dylib" "$W/pkg" "$LITERT_COMMIT" >/dev/null || return 1
  lacks "$(tools_log)" "-add_rpath @loader_path $W/pkg/libLiteRt.dylib" || return 1
  eq "$(cat "$FX/libLiteRt.dylib.rpaths")" "$(printf '/usr/lib/swift\n@loader_path')"
}
t_package_linux_sets_origin_everywhere() {
  world; mk_pkg linux-arm64 || return 1
  l="$(tools_log)"
  lacks "$l" install_name_tool && lacks "$l" codesign || return 1
  for n in liblitert-lm $LNX_PREBUILTS; do
    contains "$l" "patchelf --set-rpath \$ORIGIN $PKG/$n.so" || return 1
    eq "$(cat "$FX/$n.so.runpath")" '$ORIGIN' || return 1
  done
}
t_package_refuses_lfs_pointers() {
  world; fakes; mk_src "$W/src" macos-arm64; mk_built "$W/out/liblitert-lm.dylib" macos-arm64
  printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize 11473904\n' "$ZEROS" \
    > "$W/src/prebuilt/macos_arm64/libLiteRt.dylib"
  out="$(litert_package macos-arm64 "$W/src" "$W/out/liblitert-lm.dylib" "$W/pkg" "$LITERT_COMMIT" 2>&1)" && { echo "packaged an LFS pointer"; return 1; }
  contains "$out" "Git LFS pointer" || return 1
  [ ! -f "$W/pkg/MANIFEST.sha256" ]
}
t_package_refuses_a_missing_library_or_prebuilts() {
  world; fakes; mk_src "$W/src" macos-arm64
  out="$(litert_package macos-arm64 "$W/src" "$W/nowhere/liblitert-lm.dylib" "$W/pkg" "$LITERT_COMMIT" 2>&1)" && return 1
  contains "$out" "no built library" || return 1
  mk_built "$W/out/liblitert-lm.dylib" macos-arm64; rm -f "$W/src/prebuilt/macos_arm64"/*.dylib
  out="$(litert_package macos-arm64 "$W/src" "$W/out/liblitert-lm.dylib" "$W/pkg2" "$LITERT_COMMIT" 2>&1)" && return 1
  contains "$out" "no prebuilt libraries"
}
t_manifest_lists_every_other_file_sorted() {
  world; mk_pkg macos-arm64 || return 1
  m="$PKG/MANIFEST.sha256"
  eq "$(sed -n '2,$p' "$m" | wc -l | tr -d ' ')" 16 || return 1
  lacks "$(cat "$m")" "MANIFEST.sha256" || return 1
  eq "$(sed -n '2,$p' "$m" | sed 's/^[0-9a-f]*  //')" "$(sed -n '2,$p' "$m" | sed 's/^[0-9a-f]*  //' | LC_ALL=C sort)" || return 1
  eq "$(sed -n 2p "$m" | sed 's/^[0-9a-f]*  //')" ./LICENSE || return 1
  line="$(grep '  ./libLiteRt.dylib$' "$m")"
  eq "${line%%  *}" "$(_litert_sha256 "$PKG/libLiteRt.dylib")"
}

# ── verification ─────────────────────────────────────────────────────────────

t_verify_accepts_a_fresh_package() {
  world; mk_pkg macos-arm64 || return 1
  litert_verify "$PKG" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  eq "$LITERT_VERIFY_FILES" 16 && eq "$LITERT_VERIFY_PLATFORM" macos-arm64 && eq "$LITERT_VERIFY_COMMIT" "$LITERT_COMMIT" || return 1
  world; mk_pkg linux-arm64 || return 1
  litert_verify "$PKG" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  eq "$LITERT_VERIFY_FILES" 14
}
t_verify_catches_a_changed_file_and_inspects_nothing_after() {
  world; mk_pkg macos-arm64 || return 1
  printf 'x' >> "$PKG/libLiteRt.dylib"
  : > "$TOOL_LOG"; fake cc 'echo "cc $*" >> "$TOOL_LOG"; exit 1'
  litert_verify "$PKG" && { echo "accepted a changed file"; return 1; }
  eq "$LITERT_VERIFY_PROBLEMS" "changed since it was packaged: ./libLiteRt.dylib" || return 1
  eq "$(tools_log)" "" || { echo "inspected a package that failed its hashes: $(tools_log)"; return 1; }
}
t_verify_catches_missing_extra_and_symlinked_files() {
  world; mk_pkg macos-arm64 || return 1
  rm "$PKG/include/engine.h"; printf 'x\n' > "$PKG/extra.dylib"
  mv "$PKG/libwebgpu_dawn.dylib" "$W/dawn"; ln -s "$W/dawn" "$PKG/libwebgpu_dawn.dylib"
  litert_verify "$PKG" && return 1
  contains "$LITERT_VERIFY_PROBLEMS" "missing: ./include/engine.h" || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "not in the manifest: ./extra.dylib" || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "is a symlink: ./libwebgpu_dawn.dylib"
}
t_verify_rejects_bad_manifests() {
  world; mk_pkg macos-arm64 || return 1
  m="$PKG/MANIFEST.sha256"; cp "$m" "$W/m.orig"
  { sed -n 1p "$W/m.orig"; printf '%s  ./../../etc/hosts\n' "$ZEROS"; sed -n '2,$p' "$W/m.orig"; } > "$m"
  litert_verify "$PKG" && return 1; contains "$LITERT_VERIFY_PROBLEMS" "leaves the package: ./../../etc/hosts" || return 1
  { sed -n 1p "$W/m.orig"; echo "not a hash line"; sed -n '2,$p' "$W/m.orig"; } > "$m"
  litert_verify "$PKG" && return 1; contains "$LITERT_VERIFY_PROBLEMS" "malformed manifest line" || return 1
  sed -n '2,$p' "$W/m.orig" > "$m"
  litert_verify "$PKG" && return 1; contains "$LITERT_VERIFY_PROBLEMS" "does not start with the line" || return 1
  rm "$m"; litert_verify "$PKG" && return 1; contains "$LITERT_VERIFY_PROBLEMS" "no MANIFEST.sha256" || return 1
  litert_verify "$W/nowhere" && return 1; contains "$LITERT_VERIFY_PROBLEMS" "no such directory"
}
t_verify_requires_the_runtime_libraries_and_license() {
  world; mk_pkg macos-arm64 || return 1
  rm "$PKG/libwebgpu_dawn.dylib" "$PKG/LICENSE"
  litert_write_manifest "$PKG" "$(sed -n 1p "$PKG/MANIFEST.sha256")"
  litert_verify "$PKG" && return 1
  contains "$LITERT_VERIFY_PROBLEMS" "the package has no libwebgpu_dawn.dylib" || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "the package has no LICENSE"
}
t_verify_macho_rules() {
  world; mk_pkg macos-arm64 || return 1
  litert_verify "$PKG" || { echo "baseline: $LITERT_VERIFY_PROBLEMS"; return 1; }
  echo bazel-out/x/liblitert-lm.dylib > "$FX/liblitert-lm.dylib.id"
  printf '@loader_path\n@loader_path/../_solib_darwin_arm64/y\n' > "$FX/libLiteRt.dylib.rpaths"
  printf '/usr/lib/libSystem.B.dylib\n/opt/homebrew/lib/libzstd.1.dylib\n@rpath/libMissing.dylib\n@rpath/libswiftCore.dylib\n' > "$FX/libGemmaModelConstraintProvider.dylib.deps"
  echo x86_64 > "$FX/libLiteRtMetalAccelerator.dylib.arch"
  echo stale > "$FX/libLiteRtTopKMetalSampler.dylib.sig"
  printf '/usr/lib/swift\n' > "$FX/libLiteRtWebGpuAccelerator.dylib.rpaths"
  litert_verify "$PKG" && return 1
  p="$LITERT_VERIFY_PROBLEMS"
  contains "$p" "liblitert-lm.dylib is named bazel-out/x/liblitert-lm.dylib, not @rpath/liblitert-lm.dylib" || return 1
  contains "$p" "libLiteRt.dylib searches @loader_path/../_solib_darwin_arm64/y for libraries" || return 1
  contains "$p" "needs /opt/homebrew/lib/libzstd.1.dylib, which is neither in the package nor part of macOS" || return 1
  contains "$p" "needs @rpath/libMissing.dylib, which is not in the package" || return 1
  lacks "$p" "libswiftCore" || return 1
  contains "$p" "libLiteRtMetalAccelerator.dylib is not an arm64 Mach-O (x86_64)" || return 1
  contains "$p" "libLiteRtTopKMetalSampler.dylib has no valid code signature" || return 1
  contains "$p" "libLiteRtWebGpuAccelerator.dylib loads @rpath libraries but does not search @loader_path"
}
t_verify_elf_rules() {
  world; mk_pkg linux-arm64 || return 1
  printf '$ORIGIN/../../_solib_arm/x\n' > "$FX/libLiteRt.so.runpath"
  rm "$FX/libwebgpu_dawn.so.runpath"
  printf 'libm.so.6\nlibvulkan.so.1\n' > "$FX/libGemmaModelConstraintProvider.so.needed"
  echo "ELF 64-bit LSB shared object, x86-64, version 1 (SYSV)" > "$FX/libLiteRtTopKWebGpuSampler.so.file"
  litert_verify "$PKG" && return 1
  p="$LITERT_VERIFY_PROBLEMS"
  contains "$p" "libLiteRt.so searches '\$ORIGIN/../../_solib_arm/x', not \$ORIGIN" || return 1
  contains "$p" "libwebgpu_dawn.so searches 'nothing', not \$ORIGIN" || return 1
  contains "$p" "libLiteRtTopKWebGpuSampler.so is not a 64-bit ARM (aarch64) ELF library" || return 1
  lacks "$p" "libvulkan" || return 1
  contains "$LITERT_VERIFY_NOTES" "libGemmaModelConstraintProvider.so needs libvulkan.so.1 from the system"
}
t_verify_says_which_checks_it_could_not_make() {
  world; mk_pkg macos-arm64 || return 1
  rm "$W/bin/otool"
  litert_verify "$PKG" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  contains "$LITERT_VERIFY_NOTES" "Mach-O checks skipped: no otool" || return 1
  world; mk_pkg linux-arm64 || return 1
  rm "$W/bin/readelf" "$W/bin/file"
  litert_verify "$PKG" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  contains "$LITERT_VERIFY_NOTES" "no readelf or objdump" && contains "$LITERT_VERIFY_NOTES" "no file(1)"
}
t_load_check_only_where_the_host_can_load() {
  world; mk_pkg linux-arm64 || return 1
  fake_host Darwin arm64; litert_verify "$PKG" || return 1
  contains "$LITERT_VERIFY_NOTES" "cannot load a linux-arm64 library" || return 1
  world; mk_pkg macos-arm64 || return 1
  fake_host Darwin arm64; litert_verify "$PKG" || return 1
  contains "$LITERT_VERIFY_NOTES" "cc could not build the probe" || return 1
  # a probe that builds and then reports a load failure is a problem, not a note
  fake cc 'for o; do :; done; while [ $# -gt 0 ]; do [ "$1" = -o ] && { printf "#!/bin/sh\necho \"dlopen: image not found\" >&2; exit 1\n" > "$2"; chmod +x "$2"; }; shift; done'
  litert_verify "$PKG" && return 1
  contains "$LITERT_VERIFY_PROBLEMS" "liblitert-lm.dylib does not load: dlopen: image not found"
}

# ── state, records and the swap ──────────────────────────────────────────────

t_state_from_the_record() {
  world; mk_pkg macos-arm64 || return 1
  litert_state macos-arm64; eq "$LITERT_STATE" none || return 1
  litert_record macos-arm64 "$PKG" || return 1
  eq "$(litert_recorded_dir macos-arm64)" "$PKG" || return 1
  litert_state macos-arm64; eq "$LITERT_STATE" present && eq "$LITERT_STATE_TAG" "$(litert_tag)" && eq "$LITERT_STATE_DIR" "$PKG" || return 1
  sed "1s/$LITERT_COMMIT/0123456789abcdef0123456789abcdef01234567/" "$PKG/MANIFEST.sha256" > "$W/m"; mv "$W/m" "$PKG/MANIFEST.sha256"
  litert_state macos-arm64; eq "$LITERT_STATE" other-pin && eq "$LITERT_STATE_TAG" "$LITERT_CAPI_VERSION-01234567" || return 1
  rm -rf "$PKG"; litert_state macos-arm64; eq "$LITERT_STATE" missing || return 1
  litert_state linux-arm64; eq "$LITERT_STATE" none
}
t_install_swaps_the_whole_directory() {
  world; mkdir -p "$W/dest" "$W/stage"; printf 'old\n' > "$W/dest/old-only"; printf 'new\n' > "$W/stage/new-only"
  _litert_install "$W/stage" "$W/dest" || return 1
  [ -f "$W/dest/new-only" ] && [ ! -e "$W/dest/old-only" ] && [ ! -e "$W/stage" ] || return 1
  eq "$(names "$W" | grep -c '^dest\.old')" 0 || return 1
  _litert_install "$W/no-such-stage" "$W/dest" 2>/dev/null && return 1
  [ -f "$W/dest/new-only" ] || { echo "a failed swap lost the installed package"; return 1; }
}

# ── the desktop app ──────────────────────────────────────────────────────────

t_stage_desktop_copies_the_metal_libraries_and_license() {
  world; mk_pkg macos-arm64 || return 1
  litert_stage_desktop "$W/res/litert-lm"; eq "$?" 3 || { echo "nothing recorded should be 3"; return 1; }
  litert_record macos-arm64 "$PKG"
  mkdir -p "$W/res/litert-lm"; printf 'stale\n' > "$W/res/litert-lm/libOld.dylib"
  litert_stage_desktop "$W/res/litert-lm" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  eq "$(cd "$W/res/litert-lm" && ls | LC_ALL=C sort | tr '\n' ' ')" \
    "LICENSE MANIFEST.sha256 libGemmaModelConstraintProvider.dylib libLiteRt.dylib libLiteRtMetalAccelerator.dylib libLiteRtTopKMetalSampler.dylib liblitert-lm.dylib " || return 1
  eq "$(sed -n 1p "$W/res/litert-lm/MANIFEST.sha256")" "$(sed -n 1p "$PKG/MANIFEST.sha256")" || return 1
  litert_verify "$W/res/litert-lm" "$LITERT_DESKTOP_REQUIRED_LIBS" || { echo "staged copy: $LITERT_VERIFY_PROBLEMS"; return 1; }
  eq "$LITERT_VERIFY_FILES" 6 || return 1
  # A package must still carry WebGPU: the Orin runs on it, and so does a Mac running a package.
  litert_verify "$W/res/litert-lm" && { echo "a copy without WebGPU passed as a package"; return 1; }
  contains "$LITERT_VERIFY_PROBLEMS" "the package has no libLiteRtWebGpuAccelerator.dylib"
}
t_stage_desktop_refuses_a_bad_package() {
  world; mk_pkg macos-arm64 || return 1; litert_record macos-arm64 "$PKG"
  printf 'x' >> "$PKG/liblitert-lm.dylib"
  litert_stage_desktop "$W/res/litert-lm"; eq "$?" 1 || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "changed since it was packaged: ./liblitert-lm.dylib" || return 1
  [ ! -e "$W/res/litert-lm" ] || { echo "staged a package that failed"; return 1; }
  rm -rf "$PKG"; litert_stage_desktop "$W/res/litert-lm"; eq "$?" 1
}
t_desktop_config_names_every_shipped_library() {
  world
  y="$REPO/pond-desktop/electron-builder.yml"
  contains "$(cat "$y")" 'from: "resources/litert-lm"' || return 1
  for n in liblitert-lm $MAC_PREBUILTS; do
    case " $LITERT_DESKTOP_LEAVES_OUT " in
      *" $n "*) lacks "$(cat "$y")" "Contents/Resources/litert-lm/$n.dylib" || return 1 ;;
      *) contains "$(cat "$y")" "Contents/Resources/litert-lm/$n.dylib" || return 1 ;;
    esac
  done
}

# ── the source checkout ──────────────────────────────────────────────────────

t_source_own_clone_must_be_at_the_pin() {
  world; fakes; stubs
  c="$(mk_repo "$W/mine")"; LITERT_LM_SRC="$W/mine"
  litert_prepare_source macos-arm64 && { echo "accepted a clone that is not at the pin"; return 1; }
  contains "$(logged)" "bad: LITERT_LM_SRC is at $c; the pin is $LITERT_COMMIT" || return 1
  eq "$(git -C "$W/mine" rev-parse HEAD)" "$c" || { echo "moved a clone it does not own"; return 1; }
  LITERT_COMMIT="$c"; stubs
  litert_prepare_source macos-arm64 || { logged; return 1; }
  contains "$(logged)" "run: git -C $W/mine lfs pull --include=prebuilt/macos_arm64/*" || return 1
  contains "$(tools_log)" "git-lfs pull --include=prebuilt/macos_arm64/*"
}
t_source_managed_clone_is_cloned_and_moved_to_the_pin() {
  world; fakes; stubs
  first="$(mk_repo "$W/remote")"
  ( cd "$W/remote" && printf 'y\n' >> README && git commit -qam two )
  LITERT_REPO_URL="file://$W/remote"; LITERT_COMMIT="$first"
  litert_prepare_source linux-arm64 || { logged; return 1; }
  src="$GIAP_LITERT_HOME/src"
  eq "$(git -C "$src" rev-parse HEAD)" "$first" || return 1
  contains "$(logged)" "run: env GIT_LFS_SKIP_SMUDGE=1 git clone --filter=blob:none file://$W/remote $src" || return 1
  contains "$(logged)" "checkout --detach $first" || return 1
  lacks "$(logged)" "fetch origin" || return 1
  contains "$(logged)" "lfs pull --include=prebuilt/linux_arm64/*" || return 1
  # the next run, at the pin already, only pulls LFS objects; a dirty one is not moved
  stubs; litert_prepare_source linux-arm64 || return 1
  lacks "$(logged)" "clone" && lacks "$(logged)" "checkout" || return 1
  ( cd "$src" && git -c advice.detachedHead=false checkout -q main 2>/dev/null || git checkout -q master ) || return 1
  printf 'local change\n' >> "$src/README"
  stubs; litert_prepare_source linux-arm64 && return 1
  contains "$(logged)" "has local changes; not moving it to the pin"
}
t_source_managed_clone_from_upstream_fetches_the_pin_from_the_fork() {
  world; fakes; stubs
  mk_repo "$W/upstream" >/dev/null
  git clone -q "file://$W/upstream" "$W/fork" && ( cd "$W/fork" && printf 'giap\n' >> README && git commit -qam giap ) || return 1
  pin="$(git -C "$W/fork" rev-parse HEAD)"
  mkdir -p "$GIAP_LITERT_HOME" && git clone -q "file://$W/upstream" "$GIAP_LITERT_HOME/src" || return 1
  LITERT_REPO_URL="file://$W/fork"; LITERT_COMMIT="$pin"
  litert_prepare_source linux-arm64 || { logged; return 1; }
  eq "$(git -C "$GIAP_LITERT_HOME/src" rev-parse HEAD)" "$pin" || return 1
  contains "$(logged)" "fetch file://$W/fork $pin" || return 1
  # a fork without the pin says to push it there
  LITERT_COMMIT="$(printf '%040d' 1)"; stubs
  litert_prepare_source linux-arm64 && { echo "accepted a pin the fork does not have"; return 1; }
  contains "$(logged)" "does not have the pin; push giap-main there first"
}
t_source_dry_run_changes_nothing() {
  world; fakes; stubs; DRY_RUN=true
  litert_prepare_source macos-arm64 || { logged; return 1; }
  [ ! -e "$GIAP_LITERT_HOME" ] || { echo "a dry run created $(ls -A "$GIAP_LITERT_HOME")"; return 1; }
  contains "$(logged)" "run: env GIT_LFS_SKIP_SMUDGE=1 git clone" && contains "$(logged)" "lfs pull"
}

# ── building, with fake Bazel and Docker ─────────────────────────────────────

# A source clone of your own at the pin, shaped like LiteRT-LM.
own_src_at_pin() {
  LITERT_COMMIT="$(mk_repo "$W/LiteRT-LM")"; export LITERT_COMMIT LITERT_LM_SRC="$W/LiteRT-LM"
  mk_src "$W/LiteRT-LM" "$1"
}
t_build_macos_end_to_end() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin macos-arm64; mk_built_fixtures
  litert_build macos-arm64 || { logged; return 1; }
  dest="$(litert_package_dir macos-arm64)"
  eq "$(litert_recorded_dir macos-arm64)" "$dest" || return 1
  litert_verify "$dest" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  contains "$(tools_log)" "bazelisk build -c opt --config=macos_arm64 --define=litert_runtime_link_mode=dynamic --curses=no --color=no //c:litert-lm (in $W/LiteRT-LM)" || return 1
  contains "$(logged)" "ok: LiteRT-LM C API $LITERT_CAPI_VERSION for macos-arm64: 16 files verified in $dest" || return 1
  contains "$(logged)" "finds it through the record" || return 1
  eq "$(names "$GIAP_LITERT_HOME" | grep -c '^\.stage')" 0 || return 1
  case "$(ls -ld "$dest")" in drwxr-xr-x*) ;; *) echo "package directory mode: $(ls -ld "$dest")"; return 1 ;; esac
}
t_build_passes_distdir_and_jobs_to_bazel() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin macos-arm64; mk_built_fixtures
  mkdir -p "$W/dist"; LITERT_DISTDIR="$W/dist"; LITERT_BAZEL_JOBS=4
  litert_build macos-arm64 || { logged; return 1; }
  contains "$(tools_log)" "--color=no --distdir=$W/dist --jobs=4 //c:litert-lm" || return 1
  stubs; LITERT_DISTDIR="$W/no-such-dir"; litert_build macos-arm64 && return 1
  contains "$(logged)" "is not a directory"
}
t_build_failure_installs_nothing() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin macos-arm64; mk_built_fixtures
  export FAKE_BAZEL_FAIL=1
  litert_build macos-arm64 && return 1
  contains "$(logged)" "bad: the macos-arm64 build did not finish; nothing was installed" || return 1
  [ ! -e "$(litert_package_dir macos-arm64)" ] && [ ! -e "$(litert_record_file macos-arm64)" ] || return 1
  eq "$(names "$GIAP_LITERT_HOME" | grep -c '^\.stage')" 0
}
t_build_refuses_a_package_that_fails_verification() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin macos-arm64; mk_built_fixtures
  echo x86_64 > "$FX/libLiteRt.dylib.arch"
  litert_build macos-arm64 && return 1
  contains "$(logged)" "fails verification; nothing was installed" || return 1
  contains "$(logged)" "note: libLiteRt.dylib is not an arm64 Mach-O" || return 1
  [ ! -e "$(litert_record_file macos-arm64)" ]
}
t_build_rebuild_replaces_the_installed_package() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin macos-arm64; mk_built_fixtures
  litert_build macos-arm64 || return 1
  dest="$(litert_package_dir macos-arm64)"; printf 'x\n' > "$dest/stray"
  mk_built_fixtures; litert_build macos-arm64 || { logged; return 1; }
  [ ! -e "$dest/stray" ] && litert_verify "$dest"
}
t_build_linux_runs_the_container() {
  world; fakes; stubs; fake_host Darwin arm64; own_src_at_pin linux-arm64
  mk_built "$W/LiteRT-LM/fake-out/liblitert-lm.so" linux-arm64
  mkdir -p "$W/dist"; LITERT_DISTDIR="$W/dist"
  litert_build linux-arm64 || { logged; return 1; }
  dest="$(litert_package_dir linux-arm64)"
  eq "$(litert_recorded_dir linux-arm64)" "$dest" && litert_verify "$dest" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  a="$(cat "$W/docker-args")"
  contains "$a" "run --rm --platform linux/arm64 -v $W/LiteRT-LM:/src:ro -v $GIAP_LITERT_HOME/.stage-linux-arm64." || return 1
  contains "$a" "-v $HERE:/giap:ro -v $LITERT_DOCKER_VOLUME:/cache -v $W/dist:/distdir:ro" || return 1
  contains "$a" "$LITERT_LINUX_IMAGE bash -c" || return 1
  s="$(cat "$W/container-script.sh")"
  bash -n "$W/container-script.sh" || { echo "the container script does not parse"; return 1; }
  contains "$s" "build -c opt --config=linux_arm64 --define=litert_runtime_link_mode=dynamic --curses=no --color=no --distdir=/distdir --symlink_prefix=/ //c:litert-lm" || return 1
  contains "$s" "$LITERT_BAZELISK_LINUX_ARM64_SHA256" && contains "$s" "bazelisk/releases/download/$LITERT_BAZELISK_VERSION/bazelisk-linux-arm64" || return 1
  contains "$s" "clang-$LITERT_LINUX_CLANG" && contains "$s" "patchelf" || return 1
  contains "$s" "litert_package linux-arm64 /src \"\$bin/c/liblitert-lm.so\" /out $LITERT_COMMIT '-c opt --config=linux_arm64 --define=litert_runtime_link_mode=dynamic'" || return 1
  contains "$s" "litert_verify /out"
}
# The container's bazelisk download, run for real from the generated script with a fake curl: a
# download that does not match the pinned SHA-256 is never installed, one that does is, once.
t_container_installs_only_a_bazelisk_that_matches_its_pin() {
  world; fakes
  command -v sha256sum >/dev/null 2>&1 || fake sha256sum 'exec shasum -a 256 "$@"'
  fake curl 'echo "curl $*" >> "$TOOL_LOG"; while [ $# -gt 0 ]; do [ "$1" = -o ] && printf "%s" "$FAKE_BAZELISK" > "$2"; shift; done; exit 0'
  seg() { _litert_container_script | sed -n '/^B=/,/^fi$/p' | sed "s#/cache#$W/cache#g"; }
  export FAKE_BAZELISK="not the release"
  bash -c "set -eu; $(seg)" >/dev/null 2>"$W/err" && { echo "installed a download that fails its checksum"; return 1; }
  contains "$(cat "$W/err")" "does not match its SHA-256" || return 1
  [ -z "$(ls -A "$W/cache/bin" 2>/dev/null)" ] || { echo "left $(ls -A "$W/cache/bin")"; return 1; }
  printf '%s' "$FAKE_BAZELISK" > "$W/want"; LITERT_BAZELISK_LINUX_ARM64_SHA256="$(_litert_sha256 "$W/want")"
  bash -c "set -eu; $(seg)" >/dev/null || return 1
  b="$W/cache/bin/bazelisk-$LITERT_BAZELISK_VERSION"
  [ -x "$b" ] && eq "$(cat "$b")" "$FAKE_BAZELISK" || return 1
  : > "$TOOL_LOG"; bash -c "set -eu; $(seg)" >/dev/null || return 1
  lacks "$(tools_log)" curl
}
t_build_linux_needs_a_docker_that_answers() {
  world; fakes; stubs; export FAKE_DOCKER_DOWN=1
  litert_check_prereqs linux-arm64 && return 1
  contains "$(logged)" "docker is installed but its daemon does not answer" || return 1
  rm "$W/bin/docker"; stubs; litert_check_prereqs linux-arm64 && return 1
  contains "$(logged)" "docker is not installed"
}
t_build_macos_needs_an_apple_silicon_mac_and_its_tools() {
  world; fakes; stubs; fake_host Linux x86_64
  litert_check_prereqs macos-arm64 && return 1
  contains "$(logged)" "builds on an Apple Silicon Mac, not on Linux/x86_64" || return 1
  fake_host Darwin arm64; rm "$W/bin/bazelisk" "$W/bin/git-lfs"; stubs
  litert_check_prereqs macos-arm64 && return 1
  contains "$(logged)" "bazelisk is not installed" && contains "$(logged)" "git-lfs is not installed"
}
# On an arm64 Linux host, the Jetson, linux-arm64 builds there with the board's own flags.
t_build_mode_follows_the_host() {
  world; fakes
  fake_host Darwin arm64
  eq "$(litert_build_mode linux-arm64)" docker && eq "$(litert_build_mode macos-arm64)" native || return 1
  fake_host Linux x86_64; eq "$(litert_build_mode linux-arm64)" docker || return 1
  fake_host Linux aarch64; eq "$(litert_build_mode linux-arm64)" native || return 1
  eq "$(litert_bazel_flags linux-arm64)" "-c opt --define=litert_runtime_link_mode=dynamic --features=-parse_headers" || return 1
  eq "$(litert_bazel_flags linux-arm64 docker)" "-c opt --config=linux_arm64 --define=litert_runtime_link_mode=dynamic" || return 1
  h="$(litert_manifest_header linux-arm64 "$LITERT_COMMIT" "-c opt --features=-x")"
  contains "$h" "on linux-arm64 | bazel -c opt --features=-x //c:litert-lm" || return 1
  eq "$(_litert_header_platform "$h")" linux-arm64
}
# Native on the Jetson: no container, the host's Bazel, the board's flags in the manifest.
t_build_linux_native_end_to_end() {
  world; fakes; stubs; fake_host Linux aarch64; fake clang 'exit 0'; own_src_at_pin linux-arm64
  mk_built "$W/fixtures/liblitert-lm.so" linux-arm64
  litert_build linux-arm64 || { logged; return 1; }
  dest="$(litert_package_dir linux-arm64)"
  eq "$(litert_recorded_dir linux-arm64)" "$dest" && litert_verify "$dest" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  contains "$(tools_log)" "bazelisk build -c opt --define=litert_runtime_link_mode=dynamic --features=-parse_headers --curses=no --color=no //c:litert-lm (in $W/LiteRT-LM)" || return 1
  [ ! -e "$W/docker-args" ] || { echo "ran docker: $(cat "$W/docker-args")"; return 1; }
  contains "$(sed -n 1p "$dest/MANIFEST.sha256")" "on linux-arm64 | bazel -c opt --define=litert_runtime_link_mode=dynamic --features=-parse_headers //c:litert-lm" || return 1
  contains "$(logged)" "note: bash scripts/giap.sh litert import <that copy>"
}
t_build_linux_native_falls_back_to_bazel() {
  world; fakes; stubs; fake_host Linux aarch64; fake clang 'exit 0'; own_src_at_pin linux-arm64
  mk_built "$W/fixtures/liblitert-lm.so" linux-arm64
  mv "$W/bin/bazelisk" "$W/bin/bazel"
  litert_build linux-arm64 || { logged; return 1; }
  contains "$(tools_log)" "--features=-parse_headers" && contains "$(logged)" "run: bazel build -c opt"
}
t_build_linux_native_needs_its_tools() {
  world; fakes; stubs; fake_host Linux aarch64; rm "$W/bin/bazelisk" "$W/bin/patchelf"
  litert_check_prereqs linux-arm64 && return 1
  contains "$(logged)" "bad: neither bazelisk nor bazel is installed" || return 1
  contains "$(logged)" "bad: clang is not installed" && contains "$(logged)" "bad: patchelf is not installed" || return 1
  lacks "$(logged)" docker
}
# A package built on the Jetson, taken in on the dev machine for jetson.sh deploy.
t_import_takes_in_a_package_built_elsewhere() {
  world; stubs; mk_pkg linux-arm64
  litert_import "$PKG" || { logged; return 1; }
  dest="$(litert_package_dir linux-arm64)"
  eq "$(litert_recorded_dir linux-arm64)" "$dest" && litert_verify "$dest" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  cmp -s "$PKG/MANIFEST.sha256" "$dest/MANIFEST.sha256" || return 1
  contains "$(logged)" "files imported into $dest" || return 1
  eq "$(names "$GIAP_LITERT_HOME" | grep -c '^\.stage')" 0
}
t_import_refuses_another_pin_or_a_broken_package() {
  world; stubs; mk_pkg linux-arm64
  printf 'x\n' >> "$PKG/LICENSE"
  litert_import "$PKG" && return 1
  contains "$(logged)" "is not a package that verifies; nothing was imported" || return 1
  world; stubs; mk_pkg linux-arm64; pin="$LITERT_COMMIT"
  LITERT_COMMIT=0123456789abcdef0123456789abcdef01234567
  litert_import "$PKG" && return 1
  contains "$(logged)" "was built from LiteRT-LM $pin; the pin is $LITERT_COMMIT" || return 1
  [ ! -e "$(litert_record_file linux-arm64)" ]
}
t_build_dry_run_changes_nothing() {
  world; fakes; stubs; fake_host Darwin arm64; DRY_RUN=true; export FAKE_DOCKER_DOWN=1
  litert_build macos-arm64 || { logged; return 1; }
  litert_build linux-arm64 || { logged; return 1; }
  [ ! -e "$GIAP_LITERT_HOME" ] || { echo "a dry run created $(ls -A "$GIAP_LITERT_HOME")"; return 1; }
  eq "$(tools_log)" "" || { echo "a dry run ran: $(tools_log)"; return 1; }
  contains "$(logged)" "run: bazelisk build -c opt --config=macos_arm64" || return 1
  contains "$(logged)" "run: docker run --rm --platform linux/arm64" || return 1
  contains "$(logged)" "warn: dry run: showing the commands anyway"
}
t_build_refuses_an_unknown_platform() {
  world; stubs
  litert_build windows-x86_64; eq "$?" 2 && contains "$(logged)" "unknown platform"
}

# ── status ───────────────────────────────────────────────────────────────────

t_status_reports_each_platform() {
  world; mk_pkg macos-arm64 || return 1; stubs
  litert_status || return 1
  contains "$(logged)" "info: macos-arm64: no package recorded" || return 1
  litert_record macos-arm64 "$PKG"; stubs
  litert_status || { logged; return 1; }
  contains "$(logged)" "ok: macos-arm64: 16 files match MANIFEST.sha256 ($PKG)" || return 1
  printf 'x' >> "$PKG/libLiteRt.dylib"; stubs
  litert_status && return 1
  contains "$(logged)" "bad: macos-arm64 fails verification" && contains "$(logged)" "note: changed since it was packaged: ./libLiteRt.dylib" || return 1
  rm -rf "$PKG"; stubs; litert_status && return 1
  contains "$(logged)" "which has no package any more"
}
t_status_reports_the_deployed_package_on_linux() {
  world; mk_pkg linux-arm64 || return 1; stubs; fake_host Linux aarch64
  mkdir -p "$W/data/lib/litert-lm"; mv "$PKG" "$W/data/$(litert_device_subdir)"
  litert_status "$W/data" || { logged; return 1; }
  contains "$(logged)" "ok: deployed: 14 files match MANIFEST.sha256"
}
t_status_names_the_package_the_server_loads_when_it_is_off_the_pin() {
  world; mk_pkg linux-arm64 || return 1; stubs; fake_host Linux aarch64
  local old="$W/data/lib/litert-lm/1.0.0-b320801b"
  mkdir -p "$W/data/lib/litert-lm"; mv "$PKG" "$old"
  sed "1s/$LITERT_COMMIT/b320801b0123456789abcdef0123456789abcdef/" "$old/MANIFEST.sha256" > "$W/m"; mv "$W/m" "$old/MANIFEST.sha256"
  litert_status "$W/data"
  contains "$(logged)" "warn: the server loads $LITERT_CAPI_VERSION-b320801b; the pin is $(litert_tag)" || return 1
  lacks "$(logged)" "deployed: none"
}
t_the_server_loads_the_package_whose_library_changed_last() {
  world
  local root="$W/data/lib/litert-lm"
  mkdir -p "$root/1.0.0-aaaaaaaa" "$root/1.0.0-bbbbbbbb" "$root/1.0.0-cccccccc.metal" "$root/empty"
  : > "$root/1.0.0-aaaaaaaa/liblitert-lm.so"; : > "$root/1.0.0-bbbbbbbb/liblitert-lm.so"; : > "$root/1.0.0-cccccccc.metal/liblitert-lm.so"
  touch -t 202601010000 "$root/1.0.0-bbbbbbbb/liblitert-lm.so"
  touch -t 202602010000 "$root/1.0.0-aaaaaaaa/liblitert-lm.so" "$root/1.0.0-cccccccc.metal/liblitert-lm.so"
  eq "$(litert_device_in_use "$W/data")" "$root/1.0.0-aaaaaaaa" || return 1
  touch -t 202602010000 "$root/1.0.0-bbbbbbbb/liblitert-lm.so"
  eq "$(litert_device_in_use "$W/data")" "$root/1.0.0-bbbbbbbb" || return 1
  eq "$(litert_device_in_use "$W/nowhere")" ""
}

# ── giap.sh is the front door ────────────────────────────────────────────────

# giap.sh … against the real repo, with this test's HOME, fakes first on PATH, then the real tools.
cli() { ( cd "$REPO" && PATH="$W/bin:$ORIG_PATH" /bin/bash scripts/giap.sh "$@" 2>&1 ); }
t_cli_help_and_bad_arguments() {
  world
  contains "$(cli --help)" "giap.sh litert build" || return 1
  cli litert bogus >/dev/null;          eq "$?" 2 || return 1
  cli status extra >/dev/null;          eq "$?" 2 || return 1
  cli litert build windows >/dev/null;  eq "$?" 2 || return 1
  cli litert build a b c >/dev/null;    eq "$?" 2 || return 1
  cli --distdir >/dev/null;             eq "$?" 2
}
t_cli_status_banner_and_doctor() {
  world; mk_pkg macos-arm64 || return 1; litert_record macos-arm64 "$PKG"
  out="$(cli status)"; contains "$out" "LiteRT-LM macos-arm64 $(litert_tag) · linux-arm64 none" || return 1
  out="$(cli litert status)"; eq "$?" 0 || { echo "$out"; return 1; }
  contains "$out" "macos-arm64: 16 files match MANIFEST.sha256" || return 1
  out="$(cli doctor)"; contains "$out" "[ OK ] LiteRT-LM macos-arm64: 16 files match MANIFEST.sha256" || { echo "$out"; return 1; }
  printf 'x' >> "$PKG/libLiteRt.dylib"
  out="$(cli litert status)"; eq "$?" 1 || return 1
  out="$(cli doctor)"; eq "$?" 1 || return 1
  contains "$out" "[FAIL] LiteRT-LM macos-arm64 fails verification" || return 1
  contains "$out" "fix: bash scripts/giap.sh litert build macos-arm64" || return 1
  rm -rf "$PKG"
  out="$(cli status)"; contains "$out" "LiteRT-LM macos-arm64 MISSING" || return 1
  out="$(cli doctor)"; contains "$out" "[WARN] the recorded LiteRT-LM macos-arm64 package is gone"
}
t_cli_dry_run_build_shows_the_commands() {
  world; fakes; export FAKE_DOCKER_DOWN=1; mkdir -p "$W/dist"
  out="$(cli --dry-run litert build linux-arm64 --distdir "$W/dist")"; rc=$?
  eq "$rc" 0 || { echo "$out"; return 1; }
  contains "$out" "docker is installed but its daemon does not answer" || return 1
  contains "$out" "docker run --rm --platform linux/arm64" && contains "$out" "$W/dist:/distdir:ro" || return 1
  [ ! -e "$HOME/.giap" ] || { echo "a dry run created $HOME/.giap"; return 1; }
}

# ── native: the real tools ───────────────────────────────────────────────────

# Real Mach-O dylibs shaped like Bazel's output and Google's prebuilts: the package must come out
# with @rpath names, @loader_path rpaths and valid signatures, and dlopen from another directory.
t_native_macho() {
  case "$(uname -s)/$(uname -m)" in Darwin/arm64) ;; *) not_here "needs an Apple Silicon Mac" ;; esac
  for tool in cc install_name_tool codesign otool lipo; do command -v "$tool" >/dev/null 2>&1 || skip "needs $tool"; done
  world; export PATH="$ORIG_PATH"
  src="$W/src"; pre="$src/prebuilt/macos_arm64"; mkdir -p "$pre" "$src/c" "$W/out"
  for n in $LITERT_HEADERS; do printf '/* %s */\n' "$n" > "$src/c/$n"; done
  printf 'Apache License, Version 2.0\n' > "$src/LICENSE"
  i=0
  for n in libLiteRt libGemmaModelConstraintProvider libwebgpu_dawn libLiteRtMetalAccelerator libLiteRtTopKMetalSampler; do
    i=$((i + 1)); printf 'int marker_%s(void) { return %s; }\n' "$i" "$i" > "$W/m$i.c"
    cc -dynamiclib -o "$pre/$n.dylib" "$W/m$i.c" -install_name "@rpath/$n.dylib" || return 1
  done
  for n in libLiteRtWebGpuAccelerator libLiteRtTopKWebGpuSampler; do
    printf 'int marker_3(void); int uses_dawn_%s(void) { return marker_3(); }\n' "${#n}" > "$W/$n.c"
    cc -dynamiclib -o "$pre/$n.dylib" "$W/$n.c" -install_name "@rpath/$n.dylib" -L"$pre" -lwebgpu_dawn -Wl,-rpath,/usr/lib/swift || return 1
  done
  printf 'int marker_1(void); int marker_2(void);\nint litert_lm_engine_create(void) { return marker_1() + marker_2(); }\n' > "$W/lm.c"
  cc -dynamiclib -o "$W/out/liblitert-lm.dylib" "$W/lm.c" -L"$pre" -lLiteRt -lGemmaModelConstraintProvider \
    -install_name bazel-out/darwin_arm64-opt/bin/c/liblitert-lm.dylib \
    -Wl,-rpath,@loader_path/../_solib_darwin_arm64/_U_S_Sruntime -Wl,-rpath,@loader_path || return 1

  # What Bazel and the prebuilts give, unfixed, fails verification for the reasons it should.
  mkdir -p "$W/raw/include"; cp "$W/out/liblitert-lm.dylib" "$pre"/*.dylib "$W/raw/"; cp "$src/c"/*.h "$W/raw/include/"; cp "$src/LICENSE" "$W/raw/"
  litert_write_manifest "$W/raw" "$(litert_manifest_header macos-arm64 "$LITERT_COMMIT")"
  litert_verify "$W/raw" && { echo "accepted Bazel's raw output"; return 1; }
  contains "$LITERT_VERIFY_PROBLEMS" "liblitert-lm.dylib is named bazel-out/darwin_arm64-opt/bin/c/liblitert-lm.dylib" || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "liblitert-lm.dylib searches @loader_path/../_solib_darwin_arm64/_U_S_Sruntime" || return 1
  contains "$LITERT_VERIFY_PROBLEMS" "libLiteRtWebGpuAccelerator.dylib loads @rpath libraries but does not search @loader_path" || return 1

  litert_package macos-arm64 "$src" "$W/out/liblitert-lm.dylib" "$W/pkg" "$LITERT_COMMIT" >/dev/null 2>&1 || return 1
  litert_verify "$W/pkg" || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
  eq "$LITERT_VERIFY_NOTES" "" || { echo "a check did not run: $LITERT_VERIFY_NOTES"; return 1; }
  eq "$(otool -D "$W/pkg/liblitert-lm.dylib" | sed -n 2p)" "@rpath/liblitert-lm.dylib" || return 1
  eq "$(_litert_rpaths "$W/pkg/liblitert-lm.dylib")" "@loader_path" || return 1

  # A changed byte inside a signed page: the hashes are rewritten to match, the signature cannot be.
  cp -R "$W/pkg" "$W/resigned"
  printf '\001' | dd of="$W/resigned/libLiteRt.dylib" bs=1 seek=8192 conv=notrunc 2>/dev/null
  litert_write_manifest "$W/resigned" "$(sed -n 1p "$W/pkg/MANIFEST.sha256")"
  litert_verify "$W/resigned" && { echo "accepted a broken signature"; return 1; }
  contains "$LITERT_VERIFY_PROBLEMS" "libLiteRt.dylib has no valid code signature" || return 1

  # A changed file is caught by its hash and nothing in the package is loaded.
  printf 'x' >> "$W/pkg/libLiteRt.dylib"
  litert_verify "$W/pkg" && return 1
  eq "$LITERT_VERIFY_PROBLEMS" "changed since it was packaged: ./libLiteRt.dylib"
}

# Real ELF libraries with Bazel-style RUNPATHs: after packaging each searches exactly $ORIGIN, and
# the library loads from another directory with no LD_LIBRARY_PATH.
t_native_elf() {
  [ "$(uname -s)" = Linux ] || not_here "needs Linux"
  for tool in cc patchelf readelf file; do command -v "$tool" >/dev/null 2>&1 || skip "needs $tool"; done
  world; export PATH="$ORIG_PATH"
  src="$W/src"; pre="$src/prebuilt/linux_arm64"; mkdir -p "$pre" "$src/c" "$W/out"
  for n in $LITERT_HEADERS; do printf '/* %s */\n' "$n" > "$src/c/$n"; done
  printf 'Apache License, Version 2.0\n' > "$src/LICENSE"
  i=0
  for n in libLiteRt libGemmaModelConstraintProvider libwebgpu_dawn; do
    i=$((i + 1)); printf 'int marker_%s(void) { return %s; }\n' "$i" "$i" > "$W/m$i.c"
    cc -shared -fPIC -o "$pre/$n.so" "$W/m$i.c" -Wl,-soname,"$n.so" || return 1
  done
  for n in libLiteRtWebGpuAccelerator libLiteRtTopKWebGpuSampler; do
    printf 'int marker_3(void); int uses_dawn_%s(void) { return marker_3(); }\n' "${#n}" > "$W/$n.c"
    # shellcheck disable=SC2016
    cc -shared -fPIC -o "$pre/$n.so" "$W/$n.c" -Wl,-soname,"$n.so" -L"$pre" -lwebgpu_dawn \
      -Wl,--enable-new-dtags,-rpath,'$ORIGIN/../../_solib_arm/_U_S_Sthird_Uparty_Sdawn' || return 1
  done
  printf 'int marker_1(void); int marker_2(void);\nint litert_lm_engine_create(void) { return marker_1() + marker_2(); }\n' > "$W/lm.c"
  # shellcheck disable=SC2016
  cc -shared -fPIC -o "$W/out/liblitert-lm.so" "$W/lm.c" -L"$pre" -lLiteRt -lGemmaModelConstraintProvider \
    -Wl,--enable-new-dtags,-rpath,'$ORIGIN:$ORIGIN/../_solib_arm/_U_A_Alitert' || return 1

  litert_package linux-arm64 "$src" "$W/out/liblitert-lm.so" "$W/pkg" "$LITERT_COMMIT" >/dev/null 2>&1 || return 1
  for f in "$W/pkg"/*.so; do
    # shellcheck disable=SC2016
    eq "$(readelf -d "$f" | sed -n 's/.*(RUNPATH).*\[\(.*\)\].*/\1/p')" '$ORIGIN' || { echo "${f##*/}"; return 1; }
  done
  if litert_verify "$W/pkg"; then
    case "$(uname -m)" in aarch64|arm64) ;; *) echo "accepted $(uname -m) libraries as aarch64"; return 1 ;; esac
  else
    case "$(uname -m)" in aarch64|arm64) echo "$LITERT_VERIFY_PROBLEMS"; return 1 ;; esac
    # Off arm64 the only complaint may be the machine type, once per library.
    eq "$(printf '%s\n' "$LITERT_VERIFY_PROBLEMS" | grep -vc 'is not a 64-bit ARM (aarch64) ELF library')" 0 || { echo "$LITERT_VERIFY_PROBLEMS"; return 1; }
    eq "$(printf '%s\n' "$LITERT_VERIFY_PROBLEMS" | wc -l | tr -d ' ')" 6 || return 1
  fi
  printf '#include <dlfcn.h>\n#include <stdio.h>\nint main(int c, char **v) { void *h = dlopen(v[1], RTLD_NOW | RTLD_LOCAL); if (!h) { puts(dlerror()); return 1; } return dlsym(h, "litert_lm_engine_create") ? 0 : 2; }\n' > "$W/probe.c"
  cc -o "$W/probe" "$W/probe.c" -ldl || return 1
  out="$(cd / && env -u LD_LIBRARY_PATH "$W/probe" "$W/pkg/liblitert-lm.so")" || { echo "dlopen through \$ORIGIN failed: $out"; return 1; }
  lacks "$(ldd "$W/pkg/liblitert-lm.so")" "not found"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed, %s skipped\n' "$PASSED" "$FAILED" "$SKIPPED"
[ "$FAILED" -eq 0 ]
