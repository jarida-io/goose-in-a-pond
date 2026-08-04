#!/usr/bin/env bash
# scripts/lib/install-build.sh — Submodule init and cargo build
#
# Provides: init_submodules(), build_server()
# Sourced by install.sh — not meant to be run directly.

# ── Goose submodule ──────────────────────────────────────────────────────────
init_submodules() {
  step "2" "Goose submodule"

  cd "$REPO_DIR"
  if [ -f "goose/Cargo.toml" ]; then
    success "Goose submodule already present"
    S_SUBMODULE="ok"
  else
    log "Initializing goose submodule (first time)..."
    if git submodule update --init --recursive; then
      if [ -f "goose/Cargo.toml" ]; then
        success "Goose submodule initialized"
        S_SUBMODULE="ok"
      else
        error "Submodule init completed but goose/Cargo.toml not found"
        S_SUBMODULE="failed"
      fi
    else
      error "Submodule init failed -- check git remote access"
      S_SUBMODULE="failed"
    fi
  fi
}

# ── Build pond-server ────────────────────────────────────────────────────────
build_server() {
  local step_label
  case "$MODE" in
    dev)        step_label="Building pond-server (dev)" ;;
    minimal)    step_label="Building pond-server (minimal)" ;;
    production) step_label="Building pond-server (release)" ;;
    jetson)     step_label="Building pond-server (Jetson release)" ;;
    *)          step_label="Building pond-server" ;;
  esac

  step "4" "$step_label"

  cd "$REPO_DIR"

  # Determine build profile
  local release_flag=""
  BUILD_PROFILE="debug"
  if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ] || [ "$MODE" = "minimal" ]; then
    release_flag="--release"
    BUILD_PROFILE="release"
  fi

  # Determine cargo features
  local features=""
  if [ "$MODE" = "jetson" ] && [ "$HAS_CUDA" = true ]; then
    # `pond-server/cuda` is the alias covering BOTH the LLM and ASR adapters.
    # Passing pond-adapters-local-inference/cuda alone builds the LLM for the
    # GPU and leaves whisper on the CPU, silently.
    #
    # The name stays package-qualified because both call sites below build more
    # than one package (`--workspace`, and the four-`-p` fallback), and cargo
    # rejects a bare feature name in a multi-package build.
    features="--features pond-server/cuda"
    export CMAKE_CUDA_ARCHITECTURES=87
    export PATH="/usr/local/cuda/bin:$PATH"
    log "CUDA detected -- enabling GPU acceleration (LLM + ASR, sm_87)"
  fi

  # SQLX_OFFLINE for Jetson cross-compile compatibility
  local env_prefix=""
  if [ "$MODE" = "jetson" ]; then
    env_prefix="SQLX_OFFLINE=true"
  fi

  # Full workspace or server-only
  if [ "$FULL_BUILD" = true ]; then
    log "Full workspace build (includes Goose -- may take 10+ minutes first time)..."
    if eval "$env_prefix cargo build --workspace $release_flag $features" 2>&1 | tail -5; then
      success "Full workspace built"
      S_BUILD="ok"
    else
      warn "Full build failed -- falling back to server-only build"
      FULL_BUILD=false
    fi
  fi

  if [ "$FULL_BUILD" != true ]; then
    log "Building server crates..."
    if eval "$env_prefix cargo build -p pond-core -p pond-infra -p pond-api -p pond-server $release_flag $features" 2>&1 | tail -5; then
      success "pond-server built (${BUILD_PROFILE})"
      S_BUILD="ok"
    else
      error "Build failed -- check compiler output above"
      S_BUILD="failed"
    fi
  fi
}
