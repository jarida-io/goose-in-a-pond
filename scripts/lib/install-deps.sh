#!/usr/bin/env bash
# scripts/lib/install-deps.sh — System dependency installation
#
# Provides: install_system_deps(), ensure_llvm()
# Sourced by install.sh — not meant to be run directly.

# ── System dependencies ──────────────────────────────────────────────────────
install_system_deps() {
  step "3" "System dependencies"

  case "$OS" in
    macos)
      _install_deps_macos
      ;;
    linux)
      _install_deps_linux
      ;;
    *)
      warn "No automated dependency installer for ${OS} -- install build tools manually"
      S_DEPS="partial"
      ;;
  esac
}

_install_deps_macos() {
  if command -v brew &>/dev/null; then
    local needed=()
    command -v cmake &>/dev/null       || needed+=(cmake)
    command -v pkg-config &>/dev/null  || needed+=(pkg-config)

    if [ ${#needed[@]} -gt 0 ]; then
      log "Installing via Homebrew: ${needed[*]}"
      brew install "${needed[@]}" 2>/dev/null || warn "Some Homebrew installs failed (non-fatal)"
    fi
    success "macOS dependencies OK"
    S_DEPS="ok"
  else
    warn "Homebrew not found -- install cmake and pkg-config manually if build fails"
    S_DEPS="partial"
  fi
}

_install_deps_linux() {
  if command -v apt-get &>/dev/null; then
    log "Installing build dependencies via apt..."
    sudo apt-get update -qq 2>/dev/null || true
    if sudo apt-get install -y -qq \
        build-essential pkg-config libssl-dev libasound2-dev libdbus-1-dev cmake 2>/dev/null; then
      success "Linux dependencies installed (apt)"
      S_DEPS="ok"
    else
      warn "apt install had errors (non-fatal)"
      S_DEPS="partial"
    fi
  elif command -v dnf &>/dev/null; then
    log "Installing build dependencies via dnf..."
    if sudo dnf install -y gcc gcc-c++ openssl-devel alsa-lib-devel dbus-devel cmake pkg-config 2>/dev/null; then
      success "Linux dependencies installed (dnf)"
      S_DEPS="ok"
    else
      warn "dnf install had errors (non-fatal)"
      S_DEPS="partial"
    fi
  elif command -v pacman &>/dev/null; then
    log "Installing build dependencies via pacman..."
    if sudo pacman -S --noconfirm --needed base-devel cmake openssl alsa-lib dbus pkg-config 2>/dev/null; then
      success "Linux dependencies installed (pacman)"
      S_DEPS="ok"
    else
      warn "pacman install had errors (non-fatal)"
      S_DEPS="partial"
    fi
  else
    warn "No supported package manager (apt/dnf/pacman) -- install build deps manually"
    # shellcheck disable=SC2034  # read by print_summary in install-common.sh
    S_DEPS="partial"
  fi

  # LLVM tools are needed for production and Jetson builds
  if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
    _ensure_llvm
  fi
}

# ── LLVM tools (production/Jetson only) ──────────────────────────────────────
_ensure_llvm() {
  local tools=(clang llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-ranlib lld)
  local missing=()
  for t in "${tools[@]}"; do
    command -v "$t" &>/dev/null || missing+=("$t")
  done

  if [ ${#missing[@]} -eq 0 ]; then
    success "LLVM tools present"
    return
  fi

  warn "Missing LLVM tools: ${missing[*]}"

  if command -v apt-get &>/dev/null; then
    # Find the latest available LLVM version
    local llvm_ver
    llvm_ver=$(apt-cache search '^llvm-[0-9]+$' 2>/dev/null \
        | awk '{print $1}' | grep -oP '\d+' | sort -rn | head -1)
    llvm_ver="${llvm_ver:-17}"

    log "Installing LLVM ${llvm_ver} via apt..."
    sudo apt-get install -y \
      "clang-${llvm_ver}" \
      "llvm-${llvm_ver}" \
      "lld-${llvm_ver}" || { warn "Failed to install LLVM tools"; return; }

    # Create unversioned symlinks if missing
    for bin in clang llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-ranlib lld; do
      local versioned="/usr/bin/${bin}-${llvm_ver}"
      local unversioned="/usr/local/bin/${bin}"
      if [ -f "$versioned" ] && ! [ -e "$unversioned" ]; then
        sudo ln -sf "$versioned" "$unversioned"
      fi
    done
  elif command -v dnf &>/dev/null; then
    log "Installing LLVM via dnf..."
    sudo dnf install -y clang llvm lld || { warn "Failed to install LLVM tools"; return; }
  elif command -v pacman &>/dev/null; then
    log "Installing LLVM via pacman..."
    sudo pacman -S --noconfirm clang llvm lld || { warn "Failed to install LLVM tools"; return; }
  else
    warn "Cannot auto-install LLVM: no supported package manager. Install clang + llvm + lld manually."
    return
  fi

  success "LLVM tools installed"
}
