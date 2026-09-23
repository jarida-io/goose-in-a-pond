#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# stage-server-sidecar.sh - Build pond-server and stage it for the desktop app
#
# Produces the file electron-builder ships as an extraResource:
#
#     pond-desktop/resources/pond-server
#
# It lands in <App>.app/Contents/Resources/pond-server, which the main process
# finds via process.resourcesPath. There is no target-triple suffix any more:
# Tauri's `externalBin` was a PREFIX that its bundler globbed and de-suffixed,
# so the name had to encode the triple; electron-builder's `from:` is a literal
# path, so the name is just the name.
#
# Steps (each fails loudly if it fails):
#   1. Build the web UI (pond-desktop/dist). The pond-server build embeds
#      pond-desktop/dist at COMPILE TIME (crates/pond-api/build.rs +
#      include_dir!). Skipping this yields a binary that serves the build.rs
#      placeholder page instead of the real dashboard -- and that dashboard is
#      what the Jetson and any phone on the LAN actually see, so this ordering
#      matters well beyond the desktop app.
#   2. Build pond-server in release with RUSTFLAGS explicitly EMPTIED, with
#      the `mesh` feature always on -- the desktop app's own copy of
#      pond-server (this sidecar) needs it compiled in for the Mesh screen to
#      be anything but a permanent no-op; there is no non-mesh sidecar
#      variant, so nobody has to remember a flag to get it.
#      .cargo/config.toml sets `-C target-cpu=native`, which bakes host-CPU
#      instructions into the binary. A distributable binary built that way can
#      SIGILL on a different CPU (per AGENTS.md, the same landmine CI
#      overrides). For a shippable sidecar we must NOT specialise to this
#      build host's CPU. SQLX_OFFLINE=true keeps the build hermetic.
#   3. Copy the binary into resources/, and record a stamp of the dist it was
#      built against.
#
# The stamp exists because dist/ now feeds TWO consumers: the sidecar's
# compiled-in copy, and the renderer inside the app. If anyone rebuilds dist
# between staging and packaging, the desktop window and the LAN dashboard are
# silently different versions -- which presents as a UI bug in one place and
# not the other. scripts/verify-sidecar.sh refuses to package when they drift.
#
# Idempotent: safe to re-run; it overwrites the staged sidecar in place.
#
# Usage:
#   bash scripts/stage-server-sidecar.sh
# -----------------------------------------------------------------------------
set -euo pipefail

# Resolve the repo root from this script's location so the script works no
# matter the caller's cwd (npm runs it from pond-desktop/, humans from root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

DESKTOP_DIR="${REPO_ROOT}/pond-desktop"
RESOURCES_DIR="${DESKTOP_DIR}/resources"
SIDECAR_PATH="${RESOURCES_DIR}/pond-server"
STAMP_PATH="${RESOURCES_DIR}/.dist-stamp"
RELEASE_BIN="${REPO_ROOT}/target/release/pond-server"

fail() {
  echo "" >&2
  echo "ERROR: $*" >&2
  echo "  stage-server-sidecar.sh aborted." >&2
  exit 1
}

echo "==> Staging the pond-server sidecar for the desktop app"

# --- 1. Build the web UI (embedded into the release binary) -------------------
echo "==> [1/3] Building web UI (pond-desktop/dist) ..."
( cd "${DESKTOP_DIR}" && npm run build ) \
  || fail "web UI build failed (cd pond-desktop && npm run build). Run 'npm ci' first if deps are missing."
[ -d "${DESKTOP_DIR}/dist" ] || fail "web UI build reported success but pond-desktop/dist is missing."

# --- 2. Build pond-server (release, no host-CPU specialisation) ---------------
echo "==> [2/3] Building pond-server (release, RUSTFLAGS emptied, mesh feature on) ..."
( cd "${REPO_ROOT}" && SQLX_OFFLINE=true RUSTFLAGS="" cargo build --release -p pond-server --features mesh ) \
  || fail "cargo build of pond-server failed."
[ -f "${RELEASE_BIN}" ] || fail "cargo reported success but ${RELEASE_BIN} is missing."

bash "${SCRIPT_DIR}/build-network-helper.sh" "${RESOURCES_DIR}" || fail "network helper build failed."

# --- 3. Stage the sidecar, and stamp the dist it carries ----------------------
echo "==> [3/3] Staging sidecar -> ${SIDECAR_PATH}"
mkdir -p "${RESOURCES_DIR}"
cp "${RELEASE_BIN}" "${SIDECAR_PATH}" || fail "failed to copy the release binary into resources/."
chmod +x "${SIDECAR_PATH}"

# A content hash of dist/, so packaging can tell whether the renderer it is
# about to ship is the same one this sidecar has compiled into it.
bash "${SCRIPT_DIR}/lib/dist-stamp.sh" "${DESKTOP_DIR}/dist" > "${STAMP_PATH}" \
  || fail "could not stamp pond-desktop/dist."

echo ""
echo "OK: staged $(du -h "${SIDECAR_PATH}" | cut -f1) sidecar at:"
echo "    ${SIDECAR_PATH}"
echo ""
echo "Next: cd pond-desktop && npm run bundle:app"
