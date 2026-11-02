#!/usr/bin/env bash
# scripts/lib/install-models.sh — Model downloads and pond-server setup
#
# Provides: run_pond_setup(), download_llm()
# Sourced by install.sh — not meant to be run directly.
#
# Most model downloads are delegated to `pond-server setup` which handles
# DB init, migrations, Whisper, Piper, ONNX, and prompt templates in Rust.
# This file adds LLM provider installation (Ollama/llamafile) on top.

# ── Run pond-server setup ────────────────────────────────────────────────────
# Handles: DB init, migrations, prompt template seeding, model catalog,
#          Whisper download, Piper download, ONNX runtime.
run_pond_setup() {
  step "5" "Server setup (databases, Whisper ASR, Piper TTS, ONNX)"

  cd "$REPO_DIR" || return

  local binary="${REPO_DIR}/target/release/pond-server"
  if [ "$MODE" = "dev" ]; then
    binary="${REPO_DIR}/target/debug/pond-server"
  fi

  if [ ! -f "$binary" ]; then
    # Fall back to cargo run
    binary=""
  fi

  local setup_cmd
  if [ -n "$binary" ]; then
    setup_cmd="$binary setup --model ${WHISPER_SIZE}"
  else
    local release_flag=""
    if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ] || [ "$MODE" = "minimal" ]; then
      release_flag="--release"
    fi
    setup_cmd="cargo run -p pond-server ${release_flag} -- setup --model ${WHISPER_SIZE}"
  fi

  if [ "$NO_MODELS" = true ]; then
    log "Running setup with --no-models (DB + templates only)..."
    # pond-server setup always does DB + templates; model downloads are
    # conditional inside it but we still run it for the DB init.
    if eval "$setup_cmd" 2>&1 | grep -E '^\s*(v|!|>|#|\[)' || true; then
      S_SETUP="ok"
    fi
  else
    log "Running pond-server setup (DB, models, templates)..."
    if eval "$setup_cmd" 2>&1; then
      S_SETUP="ok"
    else
      warn "pond-server setup exited with errors -- check output above"
      S_SETUP="partial"
    fi
  fi

  # Verify results
  if [ -f "${DATA_DIR}/pond_system.db" ]; then
    S_SETUP="ok"
  else
    S_SETUP="${S_SETUP:-failed}"
  fi

  # Check Whisper
  local whisper_bin="${DATA_DIR}/bin/whisper-server"
  if [ -f "$whisper_bin" ]; then
    S_WHISPER="ok"
  elif [ "$NO_MODELS" = true ]; then
    S_WHISPER="skipped"
  else
    # shellcheck disable=SC2034  # read by print_summary in install-common.sh
    S_WHISPER="failed"
  fi

  # Check Piper
  local piper_bin="${DATA_DIR}/bin/piper"
  if [ -f "$piper_bin" ]; then
    S_PIPER="ok"
  elif [ "$NO_MODELS" = true ]; then
    S_PIPER="skipped"
  else
    # shellcheck disable=SC2034  # read by install.sh and print_summary in install-common.sh
    S_PIPER="failed"
  fi
}

# ── LLM model download ──────────────────────────────────────────────────────
# The pond runs its conversation model in process; Ollama and llamafile are opt-in flags only.
download_llm() {
  step "6" "LLM model"

  if [ "$NO_MODELS" = true ]; then
    log "Skipping LLM model (--no-models)"
    S_LLM="skipped"
    return
  fi

  case "$LLM_PROVIDER" in
    ollama)    _download_llm_ollama ;;
    llamafile) _download_llm_llamafile ;;
    *)         _choose_local_llm "$(_pond_server_bin)" ;;
  esac
}

_pond_server_bin() {
  local bin="${REPO_DIR}/target/release/pond-server"
  [ "$MODE" = "dev" ] && bin="${REPO_DIR}/target/debug/pond-server"
  if [ -x "$bin" ]; then printf '%s' "$bin"; fi
}

# GIAP's picks as name|title|size, the same files crates/pond-core/src/models/domain/curated.rs
# pins; install-models.test.sh fails when the two disagree.
LLM_PICKS="gemma-4-E4B-it-qat-UD-Q4_K_XL|Gemma 4 E4B, GIAP's main pick|4.2 GB
gemma-4-E2B-it-qat-UD-Q4_K_XL|Gemma 4 E2B, lighter and faster|2.6 GB"

_llm_interactive() { [ -t 0 ]; }

# Nothing downloads unless someone picks it here: with no conversation model the app shows the
# same picks and waits, so a non-interactive install leaves the choice to it.
_choose_local_llm() {
  local bin="$1" l4t="" choice="" default=1 i=0 name title size
  if [ -z "$bin" ]; then
    log "No conversation model yet: pick one in the app's Models page once the pond runs"
    S_LLM="pick in the app"
    return
  fi
  if [ "$IS_JETSON" = true ] && [ -r /etc/nv_tegra_release ]; then
    l4t="$(sed -n 's/.*# R\([0-9]*\).*REVISION: \([0-9.]*\).*/\1.\2/p' /etc/nv_tegra_release | head -1)"
    jetson_nvmap_state "$l4t"
    if [ "$NVMAP_STATE" = capped ]; then
      warn "L4T $l4t's stock nvmap refuses E4B's GPU weight buffer: it will not load until JetPack 6.2.2 (L4T r36.5)"
      log "bash scripts/giap.sh doctor explains the fix"
      default=3
    fi
  fi
  if ! _llm_interactive; then
    log "No conversation model yet: the app's Models page shows GIAP's picks and waits for one"
    S_LLM="pick in the app"
    return
  fi
  log "Conversation model, run on this machine (the app can change it later):"
  while IFS='|' read -r name title size; do
    i=$((i + 1)); printf '       %s) %s (%s)\n' "$i" "$title" "$size"
  done <<EOF
$LLM_PICKS
EOF
  printf '       %s) None now: pick one in the app\n' "$((i + 1))"
  read -r -p "     choose [$default]: " choice || choice=""
  name="$(printf '%s\n' "$LLM_PICKS" | awk -F'|' -v n="${choice:-$default}" 'NR == n { print $1 }')"
  if [ -z "$name" ]; then
    log "No conversation model yet: the app's Models page shows GIAP's picks and waits for one"
    S_LLM="pick in the app"
    return
  fi
  _install_local_llm "$bin" "$name"
}

# _install_local_llm <pond-server> <name>: download a pick and make it the conversation model.
_install_local_llm() {
  local bin="$1" name="$2"
  log "Downloading $name (an interrupted download resumes)..."
  if POND_DATA_DIR="$DATA_DIR" "$bin" models download gguf "$name" \
    && POND_DATA_DIR="$DATA_DIR" "$bin" models activate gguf "$name" --role chat; then
    success "LLM model: $name, in process"
    S_LLM="ok ($name)"
    if [ "$IS_JETSON" = true ]; then log "Its speculative-decoding drafter (about 57 MB) follows when it first loads"; fi
  else
    warn "$name did not download; pick it again in the app's Models page"
    S_LLM="failed"
  fi
}

# ── Ollama provider ──────────────────────────────────────────────────────────
_download_llm_ollama() {
  # Install Ollama if not present
  if ! command -v ollama &>/dev/null; then
    _install_ollama
    if ! command -v ollama &>/dev/null; then
      warn "Ollama installation failed -- install manually from https://ollama.com"
      S_LLM="failed"
      return
    fi
  fi

  success "Ollama found: $(ollama --version 2>/dev/null || echo 'installed')"

  # Ensure Ollama service is running
  _ensure_ollama_running

  # Pull default model
  log "Pulling gemma4:e2b via Ollama (~1.6 GB)..."
  if ollama pull gemma4:e2b 2>&1; then
    success "LLM model: gemma4:e2b (Ollama)"
    S_LLM="ok (ollama)"
  else
    warn "Ollama pull failed -- you can pull manually: ollama pull gemma4:e2b"
    S_LLM="failed"
  fi
}

_install_ollama() {
  case "$OS" in
    linux)
      log "Installing Ollama via official installer..."
      if curl -fsSL https://ollama.com/install.sh | sh 2>&1; then
        success "Ollama installed"
      else
        warn "Ollama auto-install failed"
      fi
      ;;
    macos)
      if command -v brew &>/dev/null; then
        echo -e "${BOLD}  ?  Install Ollama via Homebrew? [Y/n]${NC}"
        read -r -p "     " REPLY < /dev/tty
        if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
          log "Installing Ollama via Homebrew..."
          if brew install ollama 2>&1; then
            success "Ollama installed via Homebrew"
          else
            warn "Homebrew install failed -- download from https://ollama.com"
          fi
        else
          log "Skipping Ollama install"
        fi
      else
        warn "Install Ollama manually from https://ollama.com"
      fi
      ;;
    *)
      warn "Install Ollama manually from https://ollama.com"
      ;;
  esac
}

_ensure_ollama_running() {
  # Check if Ollama API is responsive
  if curl -sf "http://127.0.0.1:11434/api/version" > /dev/null 2>&1; then
    return
  fi

  # Try to start it
  if [ "$OS" = "linux" ] && command -v systemctl &>/dev/null; then
    sudo systemctl start ollama 2>/dev/null || true
    sleep 2
  elif [ "$OS" = "macos" ]; then
    # On macOS, Ollama runs as a launchd agent or standalone
    if command -v ollama &>/dev/null; then
      ollama serve &>/dev/null &
      sleep 3
    fi
  fi

  if ! curl -sf "http://127.0.0.1:11434/api/version" > /dev/null 2>&1; then
    warn "Ollama is not running -- start it manually before pulling models"
  fi
}

# ── Llamafile provider ───────────────────────────────────────────────────────
_download_llm_llamafile() {
  local llamafile_dir="${DATA_DIR}/bin"
  local llamafile_path="${llamafile_dir}/gemma-2-2b-it.llamafile"
  mkdir -p "$llamafile_dir"

  if [ -f "$llamafile_path" ]; then
    success "Llamafile already present"
    S_LLM="ok (llamafile)"
    return
  fi

  log "Downloading gemma-2-2b-it llamafile (~1.5 GB)..."
  if curl -L --progress-bar \
    "https://huggingface.co/Mozilla/gemma-2-2b-it-llamafile/resolve/main/gemma-2-2b-it.Q4_K_M.llamafile" \
    -o "$llamafile_path"; then
    chmod +x "$llamafile_path"
    success "LLM model: gemma-2-2b-it (llamafile)"
    S_LLM="ok (llamafile)"
  else
    warn "Llamafile download failed"
    # shellcheck disable=SC2034  # read by print_summary in install-common.sh
    S_LLM="failed"
  fi
}

# ── Piper TTS voice (standalone, in case pond-server setup missed it) ────────
download_piper_voice() {
  local voice_dir="${DATA_DIR}/models/tts"
  mkdir -p "$voice_dir"

  local voice_file="${voice_dir}/en_US-ryan-high.onnx"
  local config_file="${voice_dir}/en_US-ryan-high.onnx.json"

  if [ -f "$voice_file" ] && [ -f "$config_file" ]; then
    success "TTS voice already present: en_US-ryan-high"
    return
  fi

  local base_url="https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/ryan/high"
  log "Downloading en_US-ryan-high voice model (~100 MB)..."
  curl -L --progress-bar "${base_url}/en_US-ryan-high.onnx" -o "$voice_file" 2>&1 || true
  curl -sL "${base_url}/en_US-ryan-high.onnx.json" -o "$config_file" 2>&1 || true

  if [ -f "$voice_file" ] && [ -s "$voice_file" ] && [ -f "$config_file" ]; then
    success "TTS voice: en_US-ryan-high"
  else
    warn "Voice download failed -- TTS will be text-only until manually installed"
  fi
}
