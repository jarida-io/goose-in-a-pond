#!/usr/bin/env bash
# go-setup.sh: the Go that builds the remote-access helper, finding one, and getting one.
#
# Sourced, never run: it defines functions and does nothing else. Used by scripts/giap.sh (`giap.sh
# go`, the release build and the doctor).
#
# The helper (native/pondnet) is the only Go here, and its go.mod names the version it needs on its
# `go` line, or on a `toolchain` line when there is one. A Go older than 1.21 cannot read a three-part
# version there at all ("invalid go version '1.27.3': must match format 1.23"), and a newer Go below
# the pin would fetch the pinned toolchain itself, over the network, in the middle of a build. So the
# version asked for here is the pin itself: a Go at or above it builds the helper with nothing more.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`, no `${x,,}`.

# Where downloaded Go versions live, the file that remembers the one to use, and where Go is
# downloaded from. Overridable so a test never touches the real home directory or the network.
go_home()        { printf '%s' "${GIAP_GO_HOME:-$HOME/.giap/go}"; }
go_dl_url()      { printf '%s' "${GIAP_GO_DL_URL:-https://dl.google.com/go}"; }
go_record_file() { printf '%s/.path' "$(go_home)"; }

# The version native/pondnet/go.mod needs: its `toolchain` line when it has one, else its `go` line.
# Reads the go.mod under REPO_ROOT (or the one given). Fails when there is none.
go_required_version() {
  local mod="${1:-${REPO_ROOT:-.}/native/pondnet/go.mod}" v
  [ -r "$mod" ] || return 1
  v="$(awk '$1 == "toolchain" { sub(/^go/, "", $2); print $2; exit }' "$mod")"
  [ -n "$v" ] || v="$(awk '$1 == "go" { print $2; exit }' "$mod")"
  [ -n "$v" ] || return 1
  printf '%s' "$v"
}

# The version a go binary reports, without the "go" prefix: 1.27.3. Fails when it does not run.
go_version_of() {
  local out
  out="$("$1" version 2>/dev/null)" || return 1
  out="$(printf '%s' "$out" | awk '{ print $3 }')"
  out="${out#go}"
  [ -n "$out" ] || return 1
  printf '%s' "$out"
}

# One part of a dotted version, as a number; 0 when the part is missing. A pre-release ("1.28rc1")
# counts as its release, which is all the comparison below needs.
_go_part() {
  local v="$1" n="$2" i=1 part
  while [ "$i" -lt "$n" ]; do
    case "$v" in *.*) v="${v#*.}" ;; *) v="" ;; esac
    i=$((i + 1))
  done
  part="${v%%.*}"
  part="$(printf '%s' "$part" | sed 's/[^0-9].*$//')"
  printf '%s' "${part:-0}"
}

# Whether version HAVE is at or above NEED.
go_version_ok() {
  local have="$1" need="$2" n a b
  [ -n "$have" ] || return 1
  for n in 1 2 3; do
    a="$(_go_part "$have" "$n")"; b="$(_go_part "$need" "$n")"
    [ "$a" -gt "$b" ] && return 0
    [ "$a" -lt "$b" ] && return 1
  done
  return 0
}

# Every directory that may hold a go binary, one per line, in the order they are tried: the one on
# PATH, the recorded one, a download of ours, the official tarball's place, and Ubuntu's packages.
go_candidates() {
  local p rec d
  p="$(command -v go 2>/dev/null)" && [ -n "$p" ] && dirname "$p"
  rec="$(go_record_file)"
  [ -r "$rec" ] && head -1 "$rec"
  [ -x "$(go_home)/current/bin/go" ] && printf '%s\n' "$(go_home)/current/bin"
  [ -x /usr/local/go/bin/go ] && printf '%s\n' /usr/local/go/bin
  for d in /usr/lib/go-*/bin; do [ -x "$d/go" ] && printf '%s\n' "$d"; done
  return 0
}

# "version|dir" for the first Go at or above NEED, or nothing (and a failure) when none is.
go_find_ok() {
  local need="$1" dir ver
  while IFS= read -r dir; do
    [ -n "$dir" ] && [ -x "$dir/go" ] || continue
    ver="$(go_version_of "$dir/go")" || continue
    if go_version_ok "$ver" "$need"; then printf '%s|%s\n' "$ver" "$dir"; return 0; fi
  done <<EOF
$(go_candidates)
EOF
  return 1
}

# Remember DIR as the Go to use, for giap.sh's next run.
go_record() {
  local f; f="$(go_record_file)"
  mkdir -p "$(dirname "$f")" 2>/dev/null || return 1
  printf '%s\n' "$1" > "$f"
}

# Put a Go at or above the pin first on PATH for this process. Prints the version and directory as
# "version|dir"; fails, changing nothing, when there is no such Go.
go_use_repo() {
  local need line dir
  need="$(go_required_version)" || return 1
  line="$(go_find_ok "$need")" || return 1
  dir="${line#*|}"
  case ":$PATH:" in *":$dir:"*) ;; *) PATH="$dir:$PATH"; export PATH ;; esac
  printf '%s\n' "$line"
}

# linux-arm64, linux-amd64, darwin-arm64 or darwin-amd64, in go.dev's names. Fails on anything else.
go_platform() {
  case "$(uname -s)/$(uname -m)" in
    Darwin/arm64)              printf 'darwin-arm64' ;;
    Darwin/x86_64)             printf 'darwin-amd64' ;;
    Linux/x86_64)              printf 'linux-amd64' ;;
    Linux/aarch64|Linux/arm64) printf 'linux-arm64' ;;
    *) return 1 ;;
  esac
}

# SHA-256 of a file, whichever tool this machine has.
go_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else return 1; fi
}

# curl, https only (or file:// for a test's local copy of the download site), TLS 1.2 or newer.
_go_curl() {
  case "$(go_dl_url)" in
    file://*) curl -fsS --proto '=file' "$@" ;;
    *)        curl -fsS --proto '=https' --tlsv1.2 "$@" ;;
  esac
}

# Download, verify and unpack Go VERSION into $(go_home)/goVERSION, and point `current` at it.
# Prints the bin directory on success. Nothing is left half-done: the tarball is unpacked beside its
# destination and moved into place only after its SHA-256 matches the one Go publishes beside it.
# Returns 1 for no network or no such release, 2 for an unsupported platform, 3 for a checksum
# mismatch. Honours DRY_RUN=true by saying what it would do and touching nothing.
go_download() {
  local ver="$1" platform file home dest tmp want got
  platform="$(go_platform)" || { echo "no official Go build for $(uname -s)/$(uname -m)" >&2; return 2; }
  file="go${ver}.${platform}.tar.gz"
  home="$(go_home)"; dest="$home/go$ver"
  if [ "${DRY_RUN:-false}" = true ]; then
    echo "would download $(go_dl_url)/$file into $home, check it against $file.sha256, and unpack it" >&2
    return 0
  fi
  if [ -x "$dest/bin/go" ] && [ "$(go_version_of "$dest/bin/go")" = "$ver" ]; then
    ln -sfn "$dest" "$home/current" 2>/dev/null
    printf '%s/bin\n' "$dest"
    return 0
  fi
  mkdir -p "$home" || return 1
  tmp="$(mktemp -d "$home/.tmp.XXXXXX")" || return 1
  want="$(_go_curl "$(go_dl_url)/$file.sha256")" || {
    rm -rf "$tmp"; echo "could not read the checksum of $file from $(go_dl_url)" >&2; return 1; }
  want="$(printf '%s' "$want" | awk '{ print $1; exit }')"
  echo "downloading $file from $(go_dl_url) ..." >&2
  if ! _go_curl -L -o "$tmp/$file" "$(go_dl_url)/$file"; then
    rm -rf "$tmp"; echo "the download failed" >&2; return 1
  fi
  got="$(go_sha256 "$tmp/$file")" || { rm -rf "$tmp"; echo "no sha256sum or shasum to check the download with" >&2; return 1; }
  if [ "$got" != "$want" ]; then
    rm -rf "$tmp"
    echo "CHECKSUM MISMATCH for $file: $(go_dl_url) lists $want, the download is $got. Nothing was installed." >&2
    return 3
  fi
  tar -xzf "$tmp/$file" -C "$tmp" || { rm -rf "$tmp"; echo "could not unpack $file" >&2; return 1; }
  [ -x "$tmp/go/bin/go" ] || { rm -rf "$tmp"; echo "the archive has no go/bin/go" >&2; return 1; }
  rm -rf "$dest"
  mv "$tmp/go" "$dest" || { rm -rf "$tmp"; return 1; }
  rm -rf "$tmp"
  ln -sfn "$dest" "$home/current" 2>/dev/null
  printf '%s/bin\n' "$dest"
}

# Make sure a Go at or above the pin is to be had: one already installed is recorded, otherwise one
# is downloaded after asking. Uses giap.sh's ok/warn/bad/info/note/confirm. Returns 0 when one is.
go_ensure() {
  local need cur line ver dir
  need="$(go_required_version)" || { bad "no native/pondnet/go.mod to read the Go version from"; return 1; }
  cur="$(go_version_of "$(command -v go 2>/dev/null || echo go)")" || cur=""
  if [ -n "$cur" ] && go_version_ok "$cur" "$need"; then
    ok "go $cur can build the remote-access helper (go.mod asks for $need)"
    return 0
  fi
  if [ -n "$cur" ]; then warn "go $cur on PATH is older than the $need native/pondnet/go.mod asks for"
  else warn "no go on PATH (the remote-access helper needs Go $need)"; fi

  line="$(go_find_ok "$need")"
  if [ -n "$line" ]; then
    ver="${line%%|*}"; dir="${line#*|}"
    ok "found go $ver in $dir; nothing to download"
    if [ "${DRY_RUN:-false}" = true ]; then note "dry run: not recording it"; else
      go_record "$dir" && note "recorded, so giap.sh uses it whatever your shell's default is"
    fi
    return 0
  fi

  info "I can download Go $need (about 75 MB) from $(go_dl_url) into $(go_home),"
  info "checked against the SHA-256 that Go publishes beside it. Nothing outside $(go_home) changes."
  confirm "Download Go $need?" || return 1
  dir="$(go_download "$need")" || { bad "the download did not complete; nothing was installed"; return 1; }
  if [ "${DRY_RUN:-false}" = true ]; then ok "dry run: nothing was changed"; return 0; fi
  ver="$(go_version_of "$dir/go")" || { bad "the downloaded go does not run"; return 1; }
  ok "go $ver is ready in $dir"
  go_record "$dir" && note "recorded, so giap.sh uses it; in your own shell: export PATH=\"$dir:\$PATH\""
  return 0
}
