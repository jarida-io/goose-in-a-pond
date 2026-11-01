#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# deploy.sh — one-command deploy of GIAP to the Jetson Orin Nano
#
# Run from the repo root ON THE DEV MACHINE (not the Jetson):
#   bash scripts/jetson.sh deploy                # deploy origin/main
#   bash scripts/jetson.sh deploy --branch mybr  # deploy another pushed branch
#   bash scripts/jetson.sh deploy --no-restart   # build and install, but leave the service as it is
#   JETSON_HOST=other bash scripts/jetson.sh deploy     # another ssh alias or address
#   JETSON_DATA_DIR=path …   # the pond's data dir on the device, if not ~/.local/share/goose-in-a-pond
#
# The device is found once, before step 1, and every later call goes to that address
# (scripts/lib/jetson-remote.sh): by name while the name resolves, else at the address that worked
# last time, with the name's host key checked either way. No flag is needed when mDNS drops out.
#
# What it does:
#   1. Dev machine -> Jetson: the LiteRT-LM library that `giap.sh litert build
#      linux-arm64` recorded (or `giap.sh litert import` took in from a build on
#      the device), verified here, copied to
#      <data dir>/lib/litert-lm/<capi>-<commit8>/ and verified again there.
#      With none recorded this warns and skips.
#   2. Jetson: fetch + hard-reset the checkout to origin/<branch>, sync the
#      goose submodule to the pinned SHA.
#   3. Dev machine: build the web UI (Vite needs Node >= 18, which the Jetson
#      does not have) and rsync pond-desktop/dist to the Jetson — pond-server
#      embeds it at compile time (crates/pond-api/build.rs).
#   4. Jetson: release build with CUDA (sm_87; .cargo/config.toml's
#      target-cpu=native is correct for an on-device build). It runs detached there, so a lost
#      connection cannot kill it; this side follows its log and reconnects. Run the deploy again
#      after a drop and it follows the build still running, skipping steps 1-3 so nothing changes
#      under it. Below 3 GB free the pond is stopped for the build; a deploy that ends before
#      step 5 starts it again, unless the build is still running.
#   5. Jetson: restart the user-level systemd service and health-check the API.
#      --no-restart skips this, so a stopped or disabled service stays that way.
#
# Prereqs (already true on nano.local):
#   - ssh alias in ~/.ssh/config (Host nano → nano.local, key nano_jetson)
#   - loginctl enable-linger nano   (user service survives logout / starts at boot)
#   - The branch you deploy must be pushed to the Jetson's `origin` remote.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${JETSON_HOST:-nano}"
BRANCH="main"
RESTART=1
while [ $# -gt 0 ]; do
  case "$1" in
    --branch) BRANCH="$2"; shift 2 ;;
    --no-restart) RESTART=0; shift ;;
    *) echo "Unknown argument: $1"; exit 1 ;;
  esac
done

# This script lives at scripts/jetson/deploy.sh — the repo root is two levels up.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REMOTE_REPO="goose-in-a-pond"

# shellcheck source=../lib/jetson-remote.sh
source "${REPO_ROOT}/scripts/lib/jetson-remote.sh"
echo "==> Finding ${HOST}"
jetson_pin "$HOST"
trap jetson_restore_pond EXIT

# Follow a build an earlier deploy left running, and leave its checkout, library and web UI alone.
if [ "$(jetson_task_state build)" = running ]; then
  echo "==> [1-4/5] A build started by an earlier deploy is still running on ${HOST}; following it"
  jetson_follow build
else
  # First, so a package that fails its checks stops the deploy before the device changes.
  # shellcheck source=../lib/litert-setup.sh
  source "${REPO_ROOT}/scripts/lib/litert-setup.sh"
  # A pond run by agamemnon keeps its data where only agamemnon writes, which the login reaches
  # through the sudo rule `giap.sh service-account` gave it.
  REMOTE_AS=""
  REMOTE_DATA="${JETSON_DATA_DIR:-}"
  if [ "$(jetson_ssh "sed -n 's/^User=//p' /etc/systemd/system/goose-in-a-pond.service 2>/dev/null | head -1")" = agamemnon ]; then
    REMOTE_AS="sudo -n -u agamemnon"
    [ -n "$REMOTE_DATA" ] || REMOTE_DATA=/var/lib/goose-in-a-pond
  fi
  REMOTE_LITERT="${REMOTE_DATA:-.local/share/goose-in-a-pond}/$(litert_device_subdir)"
  LITERT_DIR="$(litert_recorded_dir linux-arm64)"
  echo "==> [1/5] LiteRT-LM library -> ${HOST}:${REMOTE_LITERT}"
  if [ -z "${LITERT_DIR}" ]; then
    echo "    WARNING: no linux-arm64 LiteRT-LM package is recorded here, so none is copied and"
    echo "    .litertlm models will not load on the device. Build one: bash scripts/giap.sh litert build linux-arm64"
  elif ! litert_verify "${LITERT_DIR}"; then
    echo "ERROR: the recorded LiteRT-LM package (${LITERT_DIR}) fails verification:" >&2
    printf '%s\n' "${LITERT_VERIFY_PROBLEMS}" | sed 's/^/  /' >&2
    echo "  Rebuild it: bash scripts/giap.sh litert build linux-arm64" >&2
    exit 1
  else
    jetson_ssh "${REMOTE_AS:+$REMOTE_AS }mkdir -p '${REMOTE_LITERT}'"
    jetson_rsync "${LITERT_DIR}/" "${REMOTE_LITERT}/" "${REMOTE_AS:+$REMOTE_AS rsync}"
    # The manifest's first line describes the build; every other line is a sha256sum line.
    jetson_ssh "cd '${REMOTE_LITERT}' \
      && tail -n +2 MANIFEST.sha256 | sha256sum -c --quiet --strict - \
      && missing=\$(for f in *.so; do ldd \"./\$f\" | grep 'not found' || true; done) \
      && if [ -n \"\$missing\" ]; then echo \"unresolved on the device: \$missing\" >&2; exit 1; fi"
    echo "    ${LITERT_VERIFY_FILES} files copied; sha256sum -c and ldd pass on the device"
  fi

  echo "==> [2/5] Updating Jetson checkout to origin/${BRANCH}"
  jetson_ssh "cd ~/${REMOTE_REPO} \
    && git fetch origin \
    && git checkout -q ${BRANCH} \
    && git reset --hard -q origin/${BRANCH} \
    && git submodule sync -q --recursive \
    && git submodule update --init --recursive \
    && git log --oneline -1"

  echo "==> [3/5] Building web UI locally and syncing dist"
  ( cd "${REPO_ROOT}/pond-desktop" && npm run build )
  jetson_rsync "${REPO_ROOT}/pond-desktop/dist/" "${REMOTE_REPO}/pond-desktop/dist/"

  echo "==> [4/5] Release build on the Jetson (CUDA sm_87) — this is the slow step"
  jetson_make_room_for_build "${RESTART}"
  # CMAKE_CUDA_ARCHITECTURES=87 makes ggml emit a real sm_87 cubin; without it the
  # build ships compute_80 PTX that the driver JIT-compiles at first model load.
  # whisper/cuda moves ASR decode onto the GPU (research R7: ~20s of audio in
  # ~1-2.6s instead of a multi-second all-6-core CPU burst).
  # The stamp is giap.sh's: `giap.sh update` on the device skips a rebuild of the commit it names.
  jetson_run_detached build "cd ~/${REMOTE_REPO} \
    && PATH=\$HOME/.cargo/bin:/usr/local/cuda/bin:\$PATH SQLX_OFFLINE=true \
       CMAKE_CUDA_ARCHITECTURES=87 CUDA_COMPUTE_CAP=87 \
       cargo build -p pond-server \
         --features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda \
         --release \
    && printf 'features=%s cuda_arch=87 rustflags=inherited git=%s goose=%s ui=built built=%s\\n' \
         pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda \
         \$(git rev-parse --short HEAD) \$(git -C goose rev-parse --short=9 HEAD) \
         \$(date -u +%Y-%m-%dT%H:%M:%SZ) > target/release/.giap-build-stamp"
fi

if [ "${RESTART}" -eq 0 ]; then
  JETSON_POND_SETTLED=1
  echo "==> [5/5] Service left as it is (--no-restart)"
  echo "    Start it with: ssh ${HOST} ${JETSON_POND_START_HINT}"
  exit 0
fi

echo "==> [5/5] Restarting service + health check"
# POLL, do not sleep-and-hope. The pond applies migrations, sizes the Jetson
# context, loads the GGUF embedder and pre-warms the KV prefix before it binds,
# which is tens of seconds on this board -- a fixed `sleep 4` reported HTTP 000
# and a failed deploy for a service that was starting perfectly normally.
JETSON_POND_SETTLED=1
jetson_ssh "$JETSON_POND_FNS
  pond_ctl restart || exit 1
  rm -f ~/${JETSON_REMOTE_DIR}/${JETSON_POND_STOPPED_MARK}
  for i in \$(seq 1 60); do
    code=\$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 http://127.0.0.1:8080/api/v1/health 2>/dev/null)
    if [ \"\$code\" = 200 ]; then echo \"API: HTTP 200 after \${i}0s\"; exit 0; fi
    sleep 10
  done
  echo 'API: never answered 200 within 10 minutes' >&2
  pond_status | head -12 >&2
  exit 1"

# The HTTP listener is loopback-only; phones use pinned HTTPS (normally 4443). Same port both
# ends of the tunnel, so OAuth redirects registered against it still land.
echo "Deployed. Dashboard: ssh -N -L 8080:127.0.0.1:8080 $HOST, then http://localhost:8080"
if [ "${JETSON_ADDR}" != "${JETSON_NAME}" ]; then
  echo "  (if ${JETSON_NAME} does not resolve: ssh -N -L 8080:127.0.0.1:8080 -o HostName=${JETSON_ADDR} $HOST)"
fi
