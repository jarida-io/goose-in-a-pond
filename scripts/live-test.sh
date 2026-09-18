#!/usr/bin/env bash
#
# Live-test a real pond-server against a scratch data directory.
#
# Green unit tests are not evidence that the pond starts. Every test in the
# Rust workspace runs against a database built by applying every migration to an
# empty file, in one process, with the adapter under test constructed by hand.
# None of that exercises startup ordering, migration application against a
# database that already has rows, route registration, the auth middleware, or
# the wiring in main.rs -- which is where several real defects have been.
#
#   scripts/live-test.sh              build if needed, run the API checks
#   scripts/live-test.sh --ui         also build the web UI and run the live
#                                     Playwright suite against this server
#   scripts/live-test.sh --keep       leave the server running when done
#   scripts/live-test.sh --no-build   use the existing binary
#
# Runs on macOS and Linux. Uses a scratch POND_DATA_DIR so it can never touch a
# real pond.

set -uo pipefail
set +m   # no job-control chatter when we kill background servers

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DATA_DIR="${POND_DATA_DIR:-${TMPDIR:-/tmp}/pond-live-$$}"
PORT="${POND_LIVE_PORT:-4000}"
DO_UI=0
DO_BUILD=1
KEEP=0

for arg in "$@"; do
  case "$arg" in
    --ui)       DO_UI=1 ;;
    --no-build) DO_BUILD=0 ;;
    --keep)     KEEP=1 ;;
    -h|--help)  sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

SERVER_PID=""
AUTH_PID=""
cleanup() {
  # The auth-probe server is never meant to outlive the run, even under --keep:
  # it exists only for the no-bypass pass and holding it open serves nothing.
  [ -n "$AUTH_PID" ] && kill -9 "$AUTH_PID" 2>/dev/null
  if [ -n "$SERVER_PID" ] && [ "$KEEP" -eq 0 ]; then
    kill -9 "$SERVER_PID" 2>/dev/null
  fi
  if [ "$KEEP" -eq 0 ]; then
    rm -rf "$DATA_DIR"
  else
    echo
    echo "Left running: pid $SERVER_PID on port $PORT, data in $DATA_DIR"
  fi
}
trap cleanup EXIT

say() { printf '\n=== %s ===\n' "$1"; }

# ── Server lifecycle ─────────────────────────────────────────────────────────
#
# Every helper below exists because the first macOS run of this script drove a
# DIFFERENT pond-server than the one it started, and wrote to a real pond.
#
# A `pond-server serve --native` had been running for four days on port 4000
# against the real data directory. This script assumed 4000, waited 60s for
# `.runtime_api_port`, gave up SILENTLY, kept the assumed port, and sent its
# onboarding lift -- `PUT /settings {"user_name":"LiveTest","chat_model":"mock"}`
# -- to that server. The scratch POND_DATA_DIR this script's header promises was
# never in the loop. Four real settings rows were overwritten.
#
# The rule that follows, and the reason these are three separate checks:
# never assume a port; read the one the server published; fail hard when it
# publishes none; and prove the listener is our own child before sending a
# single request to it.

# The port is written immediately after `bind_with_fallback`, which on a cold
# macOS start lands ~60s in -- exactly the old timeout, which is why it expired
# rather than failing. A missing port file is now fatal, never a fallback.
wait_for_port_file() {
  local dir="$1" label="$2" log="$3" tries="${4:-180}"
  for _ in $(seq 1 "$tries"); do
    [ -s "$dir/.runtime_api_port" ] && return 0
    sleep 1
  done
  echo "FATAL: $label never wrote .runtime_api_port after ${tries}s." >&2
  echo "       Refusing to guess a port -- guessing one is how this script" >&2
  echo "       previously wrote to a real pond." >&2
  tail -30 "$log" >&2 2>/dev/null
  return 1
}

# `lsof -t` lists the pids holding the port. Ours must be among them; anything
# else means a foreign server answers on it and every assertion would be about
# somebody else's pond.
assert_port_owned_by() {
  local port="$1" pid="$2" label="$3" holders
  holders="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | tr '\n' ' ')"
  case " $holders " in
    *" $pid "*) return 0 ;;
  esac
  echo "FATAL: port $port is held by pid(s) [${holders:-none}], not $label (pid $pid)." >&2
  echo "       Refusing to drive a pond-server this script did not start." >&2
  return 1
}

wait_for_health() {
  local port="$1" label="$2" tries="${3:-90}"
  for _ in $(seq 1 "$tries"); do
    curl -sf -o /dev/null "http://127.0.0.1:$port/api/v1/health" && return 0
    sleep 1
  done
  echo "FATAL: $label never became healthy on port $port after ${tries}s." >&2
  return 1
}

# Resolve a just-started server: its real port, its health, and that it is ours.
# Sets the global PORT_RESOLVED on success.
resolve_server() {
  local dir="$1" pid="$2" label="$3"
  # Assigned separately: within a single `local`, the default for `log` is
  # expanded before `dir` exists, and under `set -u` that aborts the run.
  local log="${4:-$dir/server.out}"
  wait_for_port_file "$dir" "$label" "$log" || return 1
  PORT_RESOLVED="$(tr -d ' \n' < "$dir/.runtime_api_port")"
  wait_for_health "$PORT_RESOLVED" "$label" || { tail -30 "$log" >&2; return 1; }
  assert_port_owned_by "$PORT_RESOLVED" "$pid" "$label" || return 1
  return 0
}

# ── Build ────────────────────────────────────────────────────────────────────
#
# RUSTFLAGS="" matches ci.yml. .cargo/config.toml sets -C target-cpu=native for
# on-device performance, and a native-CPU artifact built on one machine SIGILLs
# on another -- which presents as `signal: 4, SIGILL: illegal instruction` from
# a test binary and reads exactly like a miscompile. It is not one.
export RUSTFLAGS=""

if [ "$DO_BUILD" -eq 1 ]; then
  say "building pond-server (RUSTFLAGS empty, per ci.yml)"
  if ! cargo build -p pond-server; then
    echo "BUILD FAILED." >&2
    echo "Before reading the diff, check two things that present as code faults:" >&2
    echo "  1. disk  -- 'No space left on device', or a linker Bus error / cc failure" >&2
    echo "  2. alsa  -- pond-server needs libasound2-dev on Linux (brew has it on Mac)" >&2
    exit 1
  fi
fi

BIN="target/debug/pond-server"
[ -x "$BIN" ] || { echo "no binary at $BIN (drop --no-build?)" >&2; exit 1; }

if [ "$DO_UI" -eq 1 ]; then
  say "building the web UI"
  ( cd pond-desktop && npm ci --silent && npm run build ) || {
    echo "UI BUILD FAILED" >&2; exit 1; }
fi

# ── Start ────────────────────────────────────────────────────────────────────
#
# If startup dies at "never wrote .runtime_api_port after 180s", check this
# before reading any diff: `ensure_onnx_runtime` resolves ORT into $DATA_DIR/lib,
# and $DATA_DIR is a fresh scratch directory on every run -- so it downloads
# ~30 MB from GitHub Releases each time, and on a slow link that alone exceeds
# the timeout. It is not a hang in your feature. Export ORT_DYLIB_PATH (its
# resolution step 1) at an existing copy, e.g. the one in the real data dir:
#
#   export ORT_DYLIB_PATH="$HOME/Library/Application Support/goose-in-a-pond/lib/libonnxruntime.<ver>.dylib"
#
mkdir -p "$DATA_DIR"
say "starting pond-server against $DATA_DIR"

# `serve` shuts down on stdin EOF when detached, so stdin is held open.
# POND_DEV_ALLOW_LOOPBACK lets the checks reach protected routes without
# pairing a device. The auth section below deliberately runs a SECOND server
# without it -- with the bypass on, every auth assertion is vacuous.
POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 RUST_LOG=info \
  "$BIN" serve --port "$PORT" --static-dir pond-desktop/dist \
  > "$DATA_DIR/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!

# --port is a request, not a promise: the fallback walks 4000..4009 and the port
# it actually bound is written to .runtime_api_port. Read it, never assume it.
resolve_server "$DATA_DIR" "$SERVER_PID" "pond-server" || exit 1
PORT="$PORT_RESOLVED"
echo "healthy on port $PORT (pid $SERVER_PID, data $DATA_DIR)"

# Onboarding fronts every non-public route; lift it or everything 403s.
#
# These two writes are the ones that reached a real pond when the port was
# assumed. They stay here, but they now run only AFTER resolve_server has
# proved this port belongs to the scratch server we started.
curl -s -o /dev/null -X PUT "http://127.0.0.1:$PORT/api/v1/settings" \
  -H 'Content-Type: application/json' \
  -d '{"user_name":"LiveTest","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}'
curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/api/v1/onboard/complete"

# ── API checks ───────────────────────────────────────────────────────────────
say "API checks"
RC=0
if [ -f scripts/live_checks.py ]; then
  POND_DATA_DIR="$DATA_DIR" python3 scripts/live_checks.py || RC=$?
else
  echo "scripts/live_checks.py not present; skipping the assertion suite"
fi

# ── Migration idempotence ────────────────────────────────────────────────────
#
# A migration that only works on an empty database works exactly once, and
# every install after the first is an upgrade. Restart against the SAME
# directory, which now has rows.
say "restart against the populated database"
kill -9 "$SERVER_PID" 2>/dev/null; wait "$SERVER_PID" 2>/dev/null
# Drop the old server's port file, or resolve_server returns instantly with a
# stale port and the restart is verified against whatever now holds it.
rm -f "$DATA_DIR/.runtime_api_port"
# Stamp a lane run while nothing is holding the database, so the restart has a
# fact to remember. The lane's clock is durable as of 0059, and the only thing
# that can prove that wiring -- load at boot, into the runner, out through the
# route -- is a second process reading what a first one left behind. Writing it
# here rather than waiting for a real background pass is what makes the check
# deterministic: whether any job wins the slot during a live test depends on a
# model this pond does not have.
python3 - "$DATA_DIR" <<'LANESTAMP'
import sqlite3, sys, datetime
con = sqlite3.connect(sys.argv[1] + "/pond_system.db")
at = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=600)
con.execute(
    "INSERT INTO lane_job_runs (job, last_run_at) VALUES (?, ?) "
    "ON CONFLICT(job) DO UPDATE SET last_run_at = excluded.last_run_at",
    ("titling", at.replace(microsecond=0).isoformat().replace("+00:00", "Z")),
)
con.commit()
LANESTAMP
POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 RUST_LOG=info \
  "$BIN" serve --port "$PORT" --static-dir pond-desktop/dist \
  > "$DATA_DIR/server2.out" 2>&1 < /dev/zero &
SERVER_PID=$!
if resolve_server "$DATA_DIR" "$SERVER_PID" "pond-server (restart)" "$DATA_DIR/server2.out"; then
  PORT="$PORT_RESOLVED"
  echo "restart OK on port $PORT"
  python3 - "$DATA_DIR" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1] + "/pond_system.db")
rows = con.execute(
    "SELECT version, COUNT(*) FROM _sqlx_migrations GROUP BY version "
    "HAVING COUNT(*) > 1"
).fetchall()
print("migrations applied more than once:", rows or "none")
assert not rows, "a migration was applied twice"
PY
  # The database is not the only thing this pass has to prove survives. Since
  # PAI-2 P4 the secret store is an encrypted file, and a store the pond can
  # write but not re-open is invisible on a first start -- the process that
  # encrypted it is the one reading it back out of its own cache. Only the
  # second server can tell you.
  if [ -f scripts/live_checks.py ]; then
    POND_DATA_DIR="$DATA_DIR" python3 scripts/live_checks.py restart || RC=$?
  fi
else
  echo "RESTART FAILED -- a migration that only works on an empty database" >&2
  tail -30 "$DATA_DIR/server2.out" >&2
  RC=1
fi

# ── Auth, with the bypass OFF ────────────────────────────────────────────────
#
# Separate server on a separate port. With POND_DEV_ALLOW_LOOPBACK set, every
# assertion in this section passes regardless of what the allowlist does -- so
# running it against the server above would report the opposite of the truth.
say "auth allowlist (no token, no loopback bypass)"
AUTH_DIR="$DATA_DIR-auth"
mkdir -p "$AUTH_DIR"
POND_DATA_DIR="$AUTH_DIR" RUST_LOG=warn \
  "$BIN" serve --port "$((PORT + 20))" > "$AUTH_DIR/server.out" 2>&1 < /dev/zero &
AUTH_PID=$!

# The old version waited 60s for health and then ran the route loop REGARDLESS.
# When the server was not up yet, all five routes reported
#   FAIL  GET /settings  returned 000 with NO TOKEN
# which reads exactly like a catastrophic auth hole and is actually "no server
# answered". Same class of error as the body-predicate trap this suite warns
# about: a check that fails because the request failed reports the opposite of
# the truth. A server that does not start is now its own distinct failure.
AUTH_OK=0
if resolve_server "$AUTH_DIR" "$AUTH_PID" "auth-probe server"; then
  AUTH_OK=1
  AUTH_PORT="$PORT_RESOLVED"
else
  echo "  ERROR  the auth-probe server never came up, so the five route checks" >&2
  echo "         below did NOT run. This is not an auth finding." >&2
  RC=1
fi

if [ "$AUTH_OK" -eq 1 ]; then
  # Onboarding must be complete here too, or `require_onboarding_complete`
  # returns 403 BEFORE auth is evaluated and every route below looks protected
  # whether it is or not. Found by running this script: the first version
  # reported FAIL for /settings and /profiles on a 403, which reads like the
  # right answer and is measuring the wrong gate.
  #
  # Both writes below are themselves on the public allowlist, which is why they
  # work without a token. Since PAI-2 P7 that is no longer a hole: they are
  # classified Exposure::UntilOnboarded, so they answer only while this pond is
  # still being set up -- which it is, on a fresh $AUTH_DIR. The P7 block after
  # the route loop asserts they stop answering once this lift has landed.
  # The BEFORE half of P7's pair, and it is not decoration. Asserting only the
  # 401 after onboarding passes on a server that never started, on a route that
  # does not exist, and on an allowlist that closed PUT /settings
  # unconditionally -- which would deadlock every fresh install on a pond
  # nobody can finish setting up.
  code=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "http://127.0.0.1:$AUTH_PORT/api/v1/settings" \
    -H 'Content-Type: application/json' \
    -d '{"user_name":"AuthProbe","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}')
  if [ "$code" = "200" ]; then
    printf '  PASS  PUT    /settings             open with no token while the wizard runs\n'
  else
    printf '  FAIL  PUT    /settings returned %s BEFORE onboarding -- the wizard cannot save\n' "$code"
    RC=1
  fi
  curl -s -o /dev/null -X POST "http://127.0.0.1:$AUTH_PORT/api/v1/onboard/complete"

  for route in /settings /profiles /sessions /devices /memory; do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$AUTH_PORT/api/v1$route")
    case "$code" in
      401) printf '  PASS  GET %-10s requires a token\n' "$route" ;;
      403) printf '  SKIP  GET %-10s 403 -- onboarding guard fired before auth\n' "$route" ;;
      000) printf '  ERROR GET %-10s no response -- the server died mid-section\n' "$route"
           RC=1 ;;
      *)   printf '  FAIL  GET %-10s returned %s with NO TOKEN\n' "$route" "$code"
           RC=1 ;;
    esac
  done

  # ── PAI-2 P7: the onboarding holes close, and a reset reopens them ─────────
  #
  # This pond was onboarded a few lines above, by the two writes that had to be
  # public to do it. Those same writes must now be refused. Unit tests cover the
  # table; only this covers the middleware, the onboarding repository and a real
  # SQLite file agreeing about what state the pond is in.
  #
  # This section is in the AUTH_OK block on purpose: the loopback bypass is OFF
  # for this server. Under POND_DEV_ALLOW_LOOPBACK every assertion here passes
  # regardless of what the allowlist does, which is the opposite of the truth.
  for spec in "PUT /settings" "POST /profiles" "PATCH /profiles/live-p7" \
              "POST /onboard" "POST /onboard/complete" "POST /onboard/step/Basics"; do
    m="${spec%% *}"; route="${spec#* }"
    code=$(curl -s -o /dev/null -w '%{http_code}' -X "$m" \
      -H 'Content-Type: application/json' -d '{}' \
      "http://127.0.0.1:$AUTH_PORT/api/v1$route")
    if [ "$code" = "401" ]; then
      printf '  PASS  %-6s %-22s requires a token once onboarded\n' "$m" "$route"
    else
      printf '  FAIL  %-6s %-22s returned %s with NO TOKEN\n' "$m" "$route" "$code"
      RC=1
    fi
  done

  # ...but the status probe must NOT close. A client has to be able to ask
  # whether it needs the wizard before it has anything to authenticate with.
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$AUTH_PORT/api/v1/onboard/status")
  if [ "$code" = "200" ]; then
    printf '  PASS  GET    /onboard/status        stays public in every state\n'
  else
    printf '  FAIL  GET    /onboard/status returned %s -- a client cannot tell whether to onboard\n' "$code"
    RC=1
  fi

  # The one-way-door check, end to end. curl runs on the host, which is exactly
  # the boundary reset uses -- the same one that issues pairing codes.
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$AUTH_PORT/api/v1/onboard/reset")
  if [ "$code" = "200" ]; then
    printf '  PASS  POST   /onboard/reset         still reachable from the host\n'

    code=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "http://127.0.0.1:$AUTH_PORT/api/v1/settings" \
      -H 'Content-Type: application/json' \
      -d '{"user_name":"AuthProbe","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}')
    if [ "$code" = "200" ]; then
      printf '  PASS  PUT    /settings             reopened after the reset\n'
    else
      printf '  FAIL  PUT    /settings returned %s after a reset -- this pond is UNRECOVERABLE\n' "$code"
      RC=1
    fi

    code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$AUTH_PORT/api/v1/onboard/complete")
    if [ "$code" != "200" ]; then
      printf '  FAIL  POST   /onboard/complete returned %s after a reset\n' "$code"
      RC=1
    fi

    code=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "http://127.0.0.1:$AUTH_PORT/api/v1/settings" \
      -H 'Content-Type: application/json' -d '{"user_name":"AuthProbe"}')
    if [ "$code" = "401" ]; then
      printf '  PASS  PUT    /settings             closed again after re-onboarding\n'
    else
      printf '  FAIL  PUT    /settings returned %s -- the closure did not re-arm\n' "$code"
      RC=1
    fi
  else
    printf '  FAIL  POST   /onboard/reset returned %s from the host -- recovery is gone\n' "$code"
    RC=1
  fi
fi
kill -9 "$AUTH_PID" 2>/dev/null; wait "$AUTH_PID" 2>/dev/null
AUTH_PID=""
# Keep the probe's log when it failed to start or when --keep was asked for.
# Deleting it unconditionally is what left the first macOS failure with no
# evidence at all to diagnose from.
if [ "$AUTH_OK" -eq 1 ] && [ "$KEEP" -eq 0 ]; then
  rm -rf "$AUTH_DIR"
else
  echo "  auth-probe data kept at $AUTH_DIR"
fi

# ── Logs ─────────────────────────────────────────────────────────────────────
#
# Read them for more than your own feature. WARN and ERROR lines that were
# already there are still findings.
say "log dig"
cat "$DATA_DIR"/server*.out 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*m//g' \
  | grep -E 'WARN|ERROR|panic' \
  | sed 's/^[0-9TZ:.-]* *//' \
  | sort | uniq -c | sort -rn
if grep -qi panic "$DATA_DIR"/server*.out 2>/dev/null; then
  echo "PANIC in the server log" >&2
  RC=1
fi

# ── UI ───────────────────────────────────────────────────────────────────────
if [ "$DO_UI" -eq 1 ]; then
  say "live UI (Playwright against THIS server, no mocks)"
  ( cd pond-desktop && \
    POND_LIVE_URL="http://127.0.0.1:$PORT" \
    npx playwright test --config=playwright.live.config.ts ) || RC=1
fi

say "result"
if [ "$RC" -eq 0 ]; then
  echo "live test PASSED"
else
  echo "live test FAILED (rc=$RC)"
  echo
  # This script used to fail by design here: GET /settings and GET /profiles
  # answered with no token, which was PAI-2 P0. That landed, and the auth
  # section is expected to be GREEN from now on. A failure in it is a real
  # regression -- treat it as one, and start at is_public_route.
  echo "The auth section is expected to pass. PAI-2 P0 (the path-only"
  echo "is_public_route allowlist) has landed, so a FAIL there is a regression,"
  echo "not the known defect. Start at PUBLIC_ROUTES in pond-api/src/middleware."
fi
exit "$RC"
