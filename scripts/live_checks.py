"""Assertion suite for a running pond-server. Driven by scripts/live-test.sh.

Drives a running pond-server over real HTTP. Covers what unit and integration
tests cannot: migrations applied to a real file on disk, a restart against a
database that already has rows, route registration, the auth middleware, and
the wiring in main.rs.

Every check asserts the STATUS CODE FIRST. An earlier version of this script
reported PASS for `body.get("profile_id") is None` against an error payload,
where every lookup returns None -- a check that passes because the request
failed reports the opposite of the truth.
"""

import json
import os
import sqlite3
import subprocess
import sys

DATA_DIR = os.environ.get("POND_DATA_DIR", "/tmp/pond-live")
DB = os.path.join(DATA_DIR, "pond_system.db")
PORT_FILE = os.path.join(DATA_DIR, ".runtime_api_port")

results = []

# Written on the first pass and read back on the restart pass. The name is
# deliberately unlike anything a shell would export: `SecretRepository::has`
# consults the process environment before the store, so a plausible name would
# let this check pass on somebody's environment rather than on the store.
RESTART_CANARY_KEY = "LIVE_TEST_SECRET_ACROSS_RESTART"
RESTART_CANARY_VALUE = "live-test-restart-canary"


def check(label, ok, detail=""):
    results.append((label, bool(ok), detail))
    if len(detail) > 220:
        detail = detail[:220] + " ...(truncated)"
    print(("PASS  " if ok else "FAIL  ") + label + (("  -- " + detail) if detail else ""))
    return bool(ok)


_PORT = None


def api_port():
    """The port the server actually bound, read once from .runtime_api_port.

    This used to open the file on every single call and let a FileNotFoundError
    escape. On the first macOS run the file had not been written yet -- the
    server publishes it after `bind_with_fallback`, which is ~60s into a cold
    start -- so the suite died with a traceback partway through section_identity
    and sections P3 onward never ran at all. A missing port file is a fatal
    setup problem, not a per-check failure, so it is reported as one.
    """
    global _PORT
    if _PORT is None:
        try:
            with open(PORT_FILE) as fh:
                _PORT = fh.read().strip()
        except FileNotFoundError:
            sys.exit(
                "FATAL: %s does not exist, so there is no way to know which port the\n"
                "server bound. Never guess one -- live-test.sh guessed 4000 once and\n"
                "drove a different pond-server that happened to be holding it.\n"
                "live-test.sh is meant to resolve this before invoking these checks."
                % PORT_FILE
            )
        if not _PORT:
            sys.exit("FATAL: %s is empty." % PORT_FILE)
    return _PORT


def call(method, path, body=None, token=None):
    url = "http://127.0.0.1:%s%s" % (api_port(), path)
    cmd = ["curl", "-s", "-o", "/dev/stdout", "-w", "\n%{http_code}", "-X", method, url]
    if token:
        cmd += ["-H", "Authorization: Bearer " + token]
    if body is not None:
        cmd += ["-H", "Content-Type: application/json", "-d", json.dumps(body)]
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    raw, _, code = out.rpartition("\n")
    try:
        parsed = json.loads(raw) if raw.strip() else None
    except json.JSONDecodeError:
        parsed = raw
    return int(code), parsed


def expect(label, code, want, body, *predicates):
    """Status first, then body predicates. Returns True only if all held.

    A predicate is CALLED with the body. It used to be passed straight to
    `check`, and every caller passes a lambda -- `bool(<function>)` is True, so
    every body assertion in this file reported PASS without ever running. The
    route-ordering check that says `extraction-status` is "not a memory row" was
    one of them: it would have passed against a memory. Callables are called; a
    plain value is still read as the boolean it is.
    """
    if code != want:
        check(label, False, "HTTP %s (wanted %s): %s" % (code, want, body))
        return False
    ok = True
    for sublabel, predicate in predicates:
        verdict = predicate(body) if callable(predicate) else predicate
        ok &= check(label + " / " + sublabel, verdict, str(body))
    if not predicates:
        check(label, True)
    return ok


def db():
    """Open the pond's system database, refusing to invent one.

    `sqlite3.connect` CREATES an empty database when the path does not exist, so
    a wrong or unwritten POND_DATA_DIR surfaced as `no such table:
    _sqlx_migrations` -- which reads as "the migration did not apply" and is
    actually "there is no database here". That misdiagnosis cost a whole run.
    """
    if not os.path.exists(DB):
        sys.exit(
            "FATAL: no database at %s.\n"
            "The server either has not finished starting or is writing somewhere\n"
            "else entirely. This is NOT a migration failure -- do not read it as one."
            % DB
        )
    return sqlite3.connect(DB)


def seed_session(sid):
    con = db()
    con.execute(
        "INSERT OR REPLACE INTO sessions (id, title, created_at, updated_at) "
        "VALUES (?, 'live check', datetime('now'), datetime('now'))",
        (sid,),
    )
    con.commit()
    con.close()


def new_profile(name):
    code, body = call("POST", "/api/v1/profiles", {"display_name": name})
    if code != 201:
        check("create profile %s" % name, False, "HTTP %s: %s" % (code, body))
        return None
    return body["id"]


# ── P2: schema and provenance ────────────────────────────────────────────────


def section_schema():
    print("\n=== P2: migrations on a real database file ===")
    con = db()
    # Ask for the versions BY NAME, never for "the last three". The window form
    # was here and it broke the day 0038 landed: 0037 fell off the end of a
    # `LIMIT 3` and the check reported a missing migration that was present and
    # successful, which the very next assertion proved by querying it directly.
    # A window that has to keep up with the tree is a check that fails on
    # unrelated work -- the programme's recorded vacuity shape 4, in mirror.
    #
    # Each entry is (version, what it is, which workstream owes it). Add a row
    # when you add a migration; that is the whole maintenance burden, and it is
    # the one that fails loudly rather than silently.
    OWED_MIGRATIONS = [
        (37, "session identification", "PAI-1 P2"),
        (39, "reasoning_tokens", "PAI-5 P2"),
        (40, "session_thinking side table", "PAI-5 P6"),
    ]
    applied = {
        r[0]: r[1] for r in con.execute("SELECT version, success FROM _sqlx_migrations").fetchall()
    }
    for version, what in ((v, w) for v, w, _ in OWED_MIGRATIONS):
        check(
            "%04d applied and successful (%s)" % (version, what),
            applied.get(version) == 1,
            "row: %r" % (applied.get(version),),
        )
    for version, what, owner in OWED_MIGRATIONS:
        check(
            "%04d applied exactly once" % version,
            [
                r[0]
                for r in con.execute(
                    "SELECT version FROM _sqlx_migrations WHERE version = ?", (version,)
                )
            ]
            == [version],
            "%s owes this one" % owner,
        )
    cols = [c[1] for c in con.execute("PRAGMA table_info(sessions)").fetchall()]
    for col in ("profile_id", "identification_source", "identification_confidence"):
        check("sessions.%s present" % col, col in cols)
    trigs = [
        r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='trigger'").fetchall()
    ]
    check(
        "delete-releases-sessions trigger installed",
        "trg_profiles_delete_releases_sessions" in trigs,
        str(trigs),
    )
    con.close()


# ── P3 / P5: identification and the strength ordering ────────────────────────


def section_identity():
    print("\n=== P3: identification, provenance, and the strength ordering ===")
    seed_session("sess-identity")

    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "unidentified session",
        code,
        200,
        body,
        ("reports nobody", body and body.get("profile_id") is None),
        ("reports provenance 'unknown'", body and body.get("identification_source") == "unknown"),
    )

    code, body = call("GET", "/api/v1/sessions/does-not-exist/user")
    expect(
        "GET on an unknown session is 200, not an error",
        code,
        200,
        body,
        ("says nobody", body and body.get("profile_id") is None),
    )
    code, body = call("DELETE", "/api/v1/sessions/does-not-exist/user")
    expect("DELETE on an unknown session is 404", code, 404, body)

    jerry = new_profile("Jerry")
    liz = new_profile("Liz")
    if not (jerry and liz):
        return None, None

    # P3: explicit identification (the route that did not exist before)
    code, body = call(
        "PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": jerry}
    )
    expect(
        "PUT /sessions/{id}/user binds explicitly",
        code,
        200,
        body,
        ("bound", body and body.get("bound") is True),
        ("source is explicit", body and body.get("identification_source") == "explicit"),
    )

    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "the binding reads back",
        code,
        200,
        body,
        ("names Jerry", body and body.get("profile_id") == jerry),
        ("carries no confidence", body and body.get("confidence") is None),
    )

    # P4: the strength ordering, enforced inside the write
    con = db()
    con.execute(
        "UPDATE sessions SET identification_source = 'paired_device' WHERE id = 'sess-identity'"
    )
    con.commit()
    con.close()

    code, body = call("PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": liz})
    expect(
        "a weaker source is refused",
        code,
        200,
        body,
        ("bound is false", body and body.get("bound") is False),
        ("gives a reason", body and "reason" in body),
    )
    code, body = call("GET", "/api/v1/sessions/sess-identity/user")
    expect(
        "the stronger binding survived the attempt",
        code,
        200,
        body,
        ("still Jerry, not Liz", body and body.get("profile_id") == jerry),
        ("still paired_device", body and body.get("identification_source") == "paired_device"),
    )

    code, body = call("PUT", "/api/v1/sessions/sess-identity/user", {"profile_id": "   "})
    expect("an empty profile_id is rejected", code, 400, body)
    code, body = call("PUT", "/api/v1/sessions/no-such/user", {"profile_id": jerry})
    expect("identifying a nonexistent session is 404", code, 404, body)

    return jerry, liz


# ── P7: deletion reports, and the foreign-key trap ───────────────────────────


def section_deletion(jerry, liz):
    print("\n=== P7: deleting a member reports what went, and what stayed ===")
    if not liz:
        return

    seed_session("sess-liz")
    code, body = call("PUT", "/api/v1/sessions/sess-liz/user", {"profile_id": liz})
    if not expect("bind a session to Liz", code, 200, body,
                  ("bound", body and body.get("bound") is True)):
        return

    # Make Liz the primary member, so the dangling-reference fix is exercised.
    code, _ = call("PUT", "/api/v1/settings", {"primary_profile_id": liz})
    check("set Liz as primary member", code == 200)

    code, body = call("DELETE", "/api/v1/profiles/" + liz)
    ok = expect(
        "DELETE /profiles/{id} succeeds with a live session bound",
        code,
        200,
        body,
        ("names the member deleted", body and body.get("display_name") == "Liz"),
        ("reports a released session", body and body.get("released", {}).get("sessions") == 1),
        ("reports a memory count", body and "memories" in body.get("deleted", {})),
        ("cleared the primary setting", body and body.get("cleared_primary_profile") is True),
    )
    if not ok:
        return

    code, body = call("GET", "/api/v1/sessions/sess-liz/user")
    expect(
        "the conversation survived, stripped of its attribution",
        code,
        200,
        body,
        ("no owner", body and body.get("profile_id") is None),
        ("provenance cleared too", body and body.get("identification_source") == "unknown"),
    )

    code, body = call("GET", "/api/v1/settings")
    expect(
        "primary_profile_id no longer names a deleted member",
        code,
        200,
        body,
        ("is empty", body is not None and not body.get("primary_profile_id")),
    )

    code, body = call("DELETE", "/api/v1/profiles/" + liz)
    expect("deleting the same member again is 404", code, 404, body)
    code, body = call("DELETE", "/api/v1/profiles/never-existed")
    expect("deleting a member who never existed is 404", code, 404, body)


# ── P8: legacy rows are shared context, owned by nobody ──────────────────────


def section_legacy_rows(jerry):
    print("\n=== P8: a legacy unattributed memory is shared, not owned ===")
    con = db()
    tables = [
        r[0]
        for r in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_fragments'"
        )
    ]
    if not tables:
        check("memory_fragments table exists", False)
        con.close()
        return

    con.execute(
        "INSERT OR REPLACE INTO memory_fragments (id, profile_id, content, source, created_at) "
        "VALUES ('legacy-1', NULL, 'the spare key is under the third plant pot', 'live', datetime('now'))"
    )
    if jerry:
        con.execute(
            "INSERT OR REPLACE INTO memory_fragments (id, profile_id, content, source, created_at) "
            "VALUES ('owned-1', ?, 'my boiler code is F28', 'live', datetime('now'))",
            (jerry,),
        )
    con.commit()

    owned = con.execute(
        "SELECT COUNT(*) FROM memory_fragments WHERE profile_id = ?", (jerry,)
    ).fetchone()[0]
    shared = con.execute(
        "SELECT COUNT(*) FROM memory_fragments WHERE profile_id IS NULL"
    ).fetchone()[0]
    con.close()

    check("an owned memory is attributed", owned >= 1, "owned=%d" % owned)
    check("a legacy memory stays unattributed", shared >= 1, "shared=%d" % shared)

    # Deleting the owner must take the owned row and leave the shared one.
    if jerry:
        code, body = call("DELETE", "/api/v1/profiles/" + jerry)
        if expect("delete the owning member", code, 200, body):
            check(
                "the delete reported the owned memory",
                body.get("deleted", {}).get("memories", 0) >= 1,
                str(body.get("deleted")),
            )
            con = db()
            still_owned = con.execute(
                "SELECT COUNT(*) FROM memory_fragments WHERE id = 'owned-1'"
            ).fetchone()[0]
            still_shared = con.execute(
                "SELECT COUNT(*) FROM memory_fragments WHERE id = 'legacy-1'"
            ).fetchone()[0]
            con.close()
            check("their own memory went with them (FK cascade)", still_owned == 0)
            check(
                "the shared household memory survived",
                still_shared == 1,
                "this is the whole point of Household being a positive classification",
            )


def section_secret_store():
    """PAI-2 P4 -- the secret store on disk must be ciphertext.

    Every unit test for this builds a FileSecretRepository by hand in one
    process against an empty tempdir. That is exactly the shape of test this
    programme has been burned by: it cannot see startup wiring, and it cannot
    see the file a server that has been restarted once actually leaves behind.
    This drives the real route on the real server and then reads the bytes.

    The store and key existence checks are not padding. Without them,
    "the plaintext value is not in the file" passes trivially when there is no
    file at all -- which is the failure mode, not the success one.
    """
    store = os.path.join(DATA_DIR, "secrets.json")
    key = os.path.join(DATA_DIR, "secrets", "master.key")
    canary = "live-test-plaintext-canary"

    code, body = call(
        "PUT", "/api/v1/secrets/LIVE_TEST_SECRET", {"value": canary}
    )
    if not expect(
        "PUT /secrets/{key} stores a value",
        code,
        200,
        body,
        ("reports stored", isinstance(body, dict) and body.get("stored") is True),
    ):
        return

    if not check("secrets.json exists after a write", os.path.exists(store), store):
        return

    raw = open(store, "rb").read()
    check("the secret store is non-empty", len(raw) > 0, "%d bytes" % len(raw))
    check(
        "the secret store is a v1 envelope, not a plaintext map",
        b"giap-secret-envelope-v1" in raw,
        raw[:160].decode("utf-8", "replace"),
    )
    check(
        "the canary value is not in the file bytes",
        canary.encode() not in raw,
        "the plaintext value is on disk",
    )

    if check("the master key file exists", os.path.exists(key), key):
        mode = oct(os.stat(key).st_mode & 0o777)
        check("master.key is 0o600", mode == "0o600", mode)
        dmode = oct(os.stat(os.path.dirname(key)).st_mode & 0o777)
        check("the key directory is 0o700", dmode == "0o700", dmode)

    code, body = call("GET", "/api/v1/secrets/LIVE_TEST_SECRET/exists")
    expect(
        "the value reads back through the API",
        code,
        200,
        body,
        ("exists", isinstance(body, dict) and body.get("exists") is True),
    )

    code, body = call("DELETE", "/api/v1/secrets/LIVE_TEST_SECRET")
    expect("the live-test secret is cleaned up", code, 204, body)

    # Deliberately NOT deleted: section_secret_store_after_restart reads it back
    # from the second server. Without something surviving this pass, the restart
    # check would be asserting over a store it had just created itself.
    code, body = call(
        "PUT", "/api/v1/secrets/" + RESTART_CANARY_KEY, {"value": RESTART_CANARY_VALUE}
    )
    expect("a secret is left behind for the restart pass", code, 200, body)


def section_secret_store_after_restart():
    """PAI-2 P4 -- the second server can still open what the first one wrote.

    This is the check the phase actually rests on, and it only exists on the
    restart pass. A first start that writes an envelope proves nothing: the
    process that encrypted it is the one reading it back, out of an in-memory
    cache it never dropped. The failure this catches is a store the pond can
    write but not re-open -- which, because the old code path parsed with
    `unwrap_or_default()`, would have presented as a pond that simply forgot
    every API key and OAuth token, with no error anywhere.

    A locked store answers 503 here rather than 200-with-false, so the status
    check catches it either way.
    """
    store = os.path.join(DATA_DIR, "secrets.json")
    if not check(
        "a secret store survived the restart", os.path.exists(store), store
    ):
        return

    raw = open(store, "rb").read()
    check(
        "the store is still a v1 envelope after a restart",
        b"giap-secret-envelope-v1" in raw,
        raw[:160].decode("utf-8", "replace"),
    )
    check(
        "the restart canary is not in the file bytes",
        RESTART_CANARY_VALUE.encode() not in raw,
        "the plaintext value is on disk",
    )

    code, body = call("GET", "/api/v1/secrets/" + RESTART_CANARY_KEY + "/exists")
    expect(
        "the secret written before the restart is readable after it",
        code,
        200,
        body,
        (
            "exists",
            isinstance(body, dict) and body.get("exists") is True,
        ),
    )


def section_network_mode():
    """PAI-2 P5, first pass -- the setting exists, and a typo cannot reach it.

    Also arms the restart pass: the weather provider is built ONCE, at startup,
    from stored settings, so a server that boots without a location has
    `weather_provider = None` and answers `{"enabled": false}` with a 200 no
    matter what the gate does. Turning weather on here is what gives the second
    server something real to refuse.
    """
    print("\n=== P5: network_mode at the edge ===")

    code, body = call("GET", "/api/v1/settings")
    if not expect(
        "network_mode is on Settings and defaults to open",
        code,
        200,
        body,
        ("value", isinstance(body, dict) and body.get("network_mode") == "open"),
    ):
        return

    # NetworkMode::parse widens on an unrecognised value on purpose, so this
    # 422 is the only thing standing between a typo and a gate that is off.
    code, body = call("PUT", "/api/v1/settings", {"network_mode": "offlien"})
    expect("an unrecognised network_mode is refused at the edge", code, 422, body)

    code, body = call("GET", "/api/v1/settings")
    expect(
        "the refused value did not reach the store",
        code,
        200,
        body,
        ("still open", isinstance(body, dict) and body.get("network_mode") == "open"),
    )

    # Arm the restart pass. Coordinates are left at 0 deliberately: the adapter
    # geocodes the name, so this exercises BOTH open-meteo hosts.
    code, body = call(
        "PUT",
        "/api/v1/settings",
        {"weather_enabled": True, "weather_location_name": "Kisumu"},
    )
    expect(
        "weather is enabled for the restart pass",
        code,
        200,
        body,
        ("enabled", isinstance(body, dict) and body.get("weather_enabled") is True),
    )


def section_lane_clock_after_restart():
    """Does the lane still know when a job ran, in a process that did not run it?

    Nothing in a unit test can answer this. The runner's clock is seeded from
    `lane_job_runs` during wiring in `main.rs`, and every integration test in
    the tree builds an `AppState` with `lane: None` -- so the load, the seed and
    the route that reports it only ever meet in a real second process.

    The defect it exists for: until 0059 the clock was a `HashMap<LaneJob,
    Instant>` and a restart erased it. `select_next` reads `since_last_run:
    None` as `Duration::MAX` -- the most starved a job can be -- and
    `should_run` skips the interval floor entirely on `None`, because a job that
    has never run cannot be too soon. Both are the right reading of "never" and
    the wrong reading of "ran ten minutes ago, in the process before this one".
    So every restart handed all seven jobs a free pass through their own floors
    at once, and a daily job could run twice in ten minutes with nothing able to
    say so.

    `live-test.sh` writes titling's stamp into the database while the first
    server is down. This pass is the second server.
    """
    print("\n=== the lane clock survived the restart ===")

    code, body = call("GET", "/api/v1/lane")
    jobs = body.get("jobs", []) if isinstance(body, dict) else []
    titling = next((j for j in jobs if j.get("job") == "titling"), None)

    check(
        "the lane answers after a restart",
        code == 200 and titling is not None,
        f"HTTP {code}, {len(jobs)} jobs",
    )
    if titling is None:
        return

    age = titling.get("since_last_run_secs")
    check(
        "a run from the previous process is still dated",
        age is not None,
        f"since_last_run_secs={age!r} -- None means the clock was erased",
    )
    # The age has to be the STAMP's, not this process's. A wiring that loaded
    # the row and then stamped `now` would also report "not None" while telling
    # the scheduler the job had just run -- which is the opposite error and
    # blocks the job for a full interval instead of releasing it.
    check(
        "and dated from when it ran, not from this boot",
        isinstance(age, int) and 570 <= age <= 900,
        f"expected about 600s, got {age!r}",
    )
    # The control. Every other job's clock is genuinely empty on this pond, so
    # if the check above were passing because the route reports an age for
    # everything, this fails.
    others = [j for j in jobs if j.get("job") != "titling"]
    check(
        "a job that never ran still says so",
        all(j.get("since_last_run_secs") is None for j in others),
        f"{[(j.get('job'), j.get('since_last_run_secs')) for j in others]}",
    )


def section_network_mode_after_restart():
    """PAI-2 P5, restart pass -- does `offline` actually refuse, and say so?

    This is the assertion the phase rests on and it cannot be made anywhere
    else. The unit tests call `egress_verdict` directly; the integration test
    drives a mock repository. Only here is the gate installed by `main.rs`,
    re-installed by the real `PUT /settings` handler, and consulted by an
    adapter the server wired itself.

    A TIMEOUT IS A FAILURE OF THIS PHASE, NOT A PASS. A gate that works by
    letting the request hang until open-meteo gives up is not a gate, so the
    refusal is asserted by its MESSAGE -- which also makes this check
    independent of whether the machine running it has internet at all.

    DO NOT put a warm-up `GET /api/v1/weather` in front of the refusal as a
    "does the provider exist" control. The first version of this section did,
    and it reported PASS-then-FAIL for a reason worth remembering:
    `OpenMeteoWeatherAdapter` caches for 15 minutes, so the control served the
    second call out of memory and the gate was never consulted at all. The
    control is the message instead -- an error naming `open-meteo.com` can only
    have come from a provider that exists and was about to call it, and it
    costs no network round trip to establish.
    """
    print("\n=== P5: the gate refuses, live ===")

    code, body = call("PUT", "/api/v1/settings", {"network_mode": "offline"})
    if not expect(
        "network_mode=offline is accepted and applied without a restart",
        code,
        200,
        body,
        ("value", isinstance(body, dict) and body.get("network_mode") == "offline"),
    ):
        return

    code, body = call("GET", "/api/v1/weather")
    detail = json.dumps(body) if not isinstance(body, str) else body
    expect(
        "an offline pond refuses the weather call",
        code,
        502,
        body,
        ("names the setting", "network_mode" in detail),
        ("names the mode", "offline" in detail),
        ("names the host", "open-meteo.com" in detail),
    )

    # Put it back, and prove the gate is a gate and not a one-way door. The
    # call may still fail on a machine with no internet -- what must NOT
    # survive is the refusal.
    code, body = call("PUT", "/api/v1/settings", {"network_mode": "open"})
    if not expect(
        "network_mode=open is restored",
        code,
        200,
        body,
        ("value", isinstance(body, dict) and body.get("network_mode") == "open"),
    ):
        return

    code, body = call("GET", "/api/v1/weather")
    detail = json.dumps(body) if not isinstance(body, str) else body
    check(
        "the refusal stops when the mode is relaxed",
        "network_mode" not in detail,
        detail,
    )
    # And it is a real provider on the other side of the gate, not a stub that
    # answers `{"enabled": false}` without touching the network. A 502 is
    # accepted here and only here: this run may be on a machine with no
    # internet, and that is not this phase's failure.
    check(
        "the weather provider behind the gate is real",
        (code == 200 and isinstance(body, dict) and body.get("enabled") is True)
        or code == 502,
        "HTTP %s: %s" % (code, detail),
    )


def section_redaction():
    """PAI-2 P3 -- does the redactor sit on the write path production wired?

    Every redaction unit test builds the decorator by hand. This one goes in
    over HTTP, at the default level, with no configuration, and reads the row
    back out of SQLite -- so it fails if `run_server` binds the raw
    `SqliteMemoryRepository`, which is the one thing those unit tests cannot
    see and the one thing that compiles perfectly either way.

    Both halves matter. A redactor that eats ordinary prose is worse than none,
    because the user stops trusting the transcript and turns it off, so the
    negative is asserted BYTE-FOR-BYTE rather than by absence of a placeholder.
    """
    print("\n=== P3: redaction on the real memory write path ===")

    key = "sk-livecheckabcdefghijklmnopqrstuv"
    prose = "I grew up near B2 3AM and the hub is at 192.168.1.50."
    content = "stripe key %s -- %s" % (key, prose)

    code, body = call(
        "POST", "/api/v1/memories", {"content": content, "source": "live-check"}
    )
    # 201, not 200. The handler echoes back the fragment it BUILT, which still
    # holds the credential -- the decorator redacts on the way into the store,
    # and it is the stored row that is asserted on below. Reading the response
    # body here would have reported the opposite of the truth.
    if not expect("a memory with a credential in it is accepted", code, 201, body):
        return

    con = db()
    rows = con.execute(
        "SELECT content FROM memory_fragments WHERE source = 'live-check'"
    ).fetchall()
    con.close()

    if not check("the fragment reached pond_system.db", len(rows) == 1, str(rows)):
        return
    stored = rows[0][0]

    check(
        "the credential is not in the stored row",
        key not in stored,
        "the raw key is in pond_system.db: %s" % stored,
    )
    check(
        "the credential was replaced, not deleted",
        "[redacted:api-key]" in stored,
        stored,
    )
    check(
        "ordinary prose came through byte for byte",
        stored.endswith(prose),
        "prose was mangled -- stored: %s" % stored,
    )


def section_policy_telemetry():
    """PAI-2 P8a -- is the would-deny telemetry wired by `run_server`?

    The unit tests construct `SqliteSecurityPolicy` themselves and hand it to a
    router they built. They cannot answer whether `main.rs` bound a policy at
    all, whether it bound one with an event log behind it, or whether the report
    route is registered on the running binary. That is the same gap
    `section_redaction` exists to close, and it is where PAI-2's defects have
    actually been.

    **What this can and cannot reach, said plainly.** live-test.sh starts this
    server with POND_DEV_ALLOW_LOOPBACK, so the middleware attaches
    `Principal::loopback()` -- and `is_identity_assertion_proven` deliberately
    permits loopback ("whoever is at the console already has the box"). So the
    decision this section takes is an ALLOW, and a would-deny is not producible
    from here. That is not a weakness in the rule: a remote caller presenting a
    real handshake token gets `Principal::token`, which proves nothing, and
    would-denies. It does mean the number this section moves is `allow`, and
    claiming otherwise would be the "fixture unreachable in production" mistake
    in reverse.

    What it therefore proves: the route exists on the real binary, the policy is
    bound with a live event sink, and the report reads the `verdict` ATTRIBUTE
    off the stored event rather than a substring of the action string.
    """
    print("\n=== P8a: policy telemetry on the running binary ===")

    code, before = call("GET", "/api/v1/security/policy-report?window=day")
    if not expect(
        "the policy report is registered and answers",
        code,
        200,
        before,
        (
            "has an events block",
            isinstance(before, dict) and isinstance(before.get("events"), dict),
        ),
        (
            "has a process block",
            isinstance(before, dict) and isinstance(before.get("process"), dict),
        ),
        (
            "the event log is actually bound",
            isinstance(before, dict)
            and before.get("events", {}).get("available") is True,
        ),
    ):
        return

    code, body = call("GET", "/api/v1/security/policy-report?window=forever")
    expect("an unrecognised window is refused, not widened", code, 400, body)

    seed_session("sess-policy-telemetry")
    who = new_profile("Telemetry")
    if not who:
        return

    code, body = call(
        "PUT", "/api/v1/sessions/sess-policy-telemetry/user", {"profile_id": who}
    )
    if not expect(
        "an identity assertion from the console is permitted",
        code,
        200,
        body,
        ("bound", isinstance(body, dict) and body.get("bound") is True),
    ):
        return

    code, after = call("GET", "/api/v1/security/policy-report?window=day")
    if not expect("the report answers after the decision", code, 200, after):
        return

    def delta(block, key):
        return after[block][key] - before[block][key]

    check(
        "the decision reached the durable event half",
        delta("events", "allow") == 1,
        "events allow delta %s (before %s, after %s)"
        % (delta("events", "allow"), before["events"], after["events"]),
    )
    check(
        "the decision reached the process counters",
        delta("process", "allow") == 1,
        "process allow delta %s (before %s, after %s)"
        % (delta("process", "allow"), before["process"], after["process"]),
    )
    # If the verdict ever rides the action string again, the attribute the
    # report groups on goes missing and every row lands here instead. A zero
    # would_deny alone would not have caught that -- it is zero anyway.
    check(
        "no audit row was unreadable to the report",
        after["events"]["unclassified"] == 0,
        "unclassified %s -- an audit event carried no verdict attribute: %s"
        % (after["events"]["unclassified"], after["events"]),
    )
    check(
        "the count is not silently capped",
        after["events"]["truncated"] is False,
        str(after["events"]),
    )


# ── The batch memory-extraction engine ───────────────────────────────────────


def section_memory_extraction():
    """Phase 2: nothing else extracts memories now, so the engine has to answer.

    Three things this can see and no unit test can. The route is registered --
    it sits before `/memories/{id}` in the router and axum would otherwise match
    `extraction-status` as an id. The cursor columns migration 0056 added are on
    a real database file with rows already in it. And the engine reports its own
    absence honestly: this pond has no embedding model, so it must say so rather
    than look identical to a pond with nothing left to read.
    """
    print("\n=== memory extraction: the engine answers for itself ===")

    code, body = call("GET", "/api/v1/memories/extraction-status")
    expect(
        "the extraction status route is registered",
        code,
        200,
        body,
        ("answers with an object", lambda b: isinstance(b, dict)),
        # The ordering trap: `/memories/{id}` would match "extraction-status" as
        # an id and return a memory, or a 404, rather than this.
        ("is not a memory row", lambda b: "sessions_total" in b),
        ("says whether the engine is running here", lambda b: "running" in b),
        ("says why it is not, when it is not", lambda b: "blocked_on" in b),
        # The loss that is invisible from every other angle: a pond with more
        # than one member never mines a conversation nobody has identified, and
        # the voice child cannot be identified at all -- it is a separate
        # process with no request, so none of the three things that bind a
        # session to a member can reach it. The count has to be on the wire or
        # the household has no way to see it.
        (
            "says how many conversations nobody can name",
            lambda b: "unattributed_sessions" in b,
        ),
    )

    if not isinstance(body, dict):
        return

    # A pond with no embedder must SAY it is not extracting. The failure this
    # guards is the quiet one: reading nothing looks exactly like having nothing
    # left to read, and the difference is a household's whole history.
    if not body.get("running"):
        check(
            "a pond with no engine says so rather than reporting a zeroed pass",
            body.get("blocked_on") is not None or body.get("last_pass_at") is None,
            "running=%s blocked_on=%r" % (body.get("running"), body.get("blocked_on")),
        )

    # The cursor is a real column on a real row, not a default a mock returned.
    seed_session("sess-extraction-cursor")
    con = db()
    cols = {r[1] for r in con.execute("PRAGMA table_info(sessions)")}
    con.close()
    for col in ("extracted_through_id", "extracted_at", "extraction_attempts"):
        check(
            "migration 0056 put %s on the sessions table" % col,
            col in cols,
            "columns: %s" % sorted(cols),
        )

    code, body = call("GET", "/api/v1/memories/extraction-status")
    check(
        "a newly seeded conversation is counted as still to read",
        code == 200 and body.get("sessions_pending", 0) >= 1,
        "HTTP %s: %s" % (code, body),
    )

    # Where a refused date goes. The gate refuses any memory carrying a one-off
    # calendar date on the understanding that the date is kept as a reminder
    # instead, and for one release nothing was: the candidate was counted and
    # dropped. A unit test builds this table by applying every migration to an
    # empty file; this asks the real database file, after a restart, whether the
    # table and its dedup guard are actually there.
    con = db()
    tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    check(
        "migration 0057 created the reminders table",
        "reminders" in tables,
        "tables: %s" % sorted(tables),
    )
    if "reminders" in tables:
        cols = {r[1] for r in con.execute("PRAGMA table_info(reminders)")}
        for col in ("about", "when_said", "session_id", "window_id", "disposition"):
            check(
                "the reminders table carries %s" % col,
                col in cols,
                "columns: %s" % sorted(cols),
            )
        # The re-walk guard. Without it the engine files the same reminder again
        # every time a cleared cursor sends it back over a window it has read.
        uniques = [
            r[1]
            for r in con.execute("PRAGMA index_list(reminders)")
            if r[2] == 1
        ]
        unique_cols = set()
        for name in uniques:
            unique_cols.update(r[2] for r in con.execute("PRAGMA index_info(%s)" % name))
        check(
            "the reminders table refuses the same window's reminder twice",
            {"window_id", "about_key"} <= unique_cols,
            "unique indexes over: %s" % sorted(unique_cols),
        )
    con.close()

    # The two numbers that say whether the date rule is costing anything. The
    # panel used to imply that dates_lost = 0 meant the date had been moved,
    # while nothing moved it -- so the counters have to be on the wire before
    # anything can claim that again.
    check(
        "the status route says what the last pass kept and what it lost",
        "last_pass_reminders_written" in body and "last_pass_reminders_lost" in body,
        str(body),
    )


# ── The reminders surface ────────────────────────────────────────────────────


def section_inference_lane():
    """The six background jobs answer for themselves, and can be run by hand.

    Nothing below is visible to a unit test. The two routes have to be
    REGISTERED, and `/lane/jobs/{job}/run` sits one path segment away from the
    same trap `extraction-status` fell into. The lane has to be WIRED into
    `AppState` -- every integration test in the tree builds one with `lane:
    None`, so the only place `Some` is ever exercised is a real server. And the
    job list has to come back with all six whether or not their loops spawned on
    this pond, which is a fact about `claim` being called at spawn time.

    The defect this surface exists for: on a real pond the memory engine had
    never completed a pass -- 958 conversations, zero cursors -- and no surface
    anywhere could say whether it was switched off, waiting, or losing a
    tie-break it would never win.
    """
    print("\n=== the inference lane: the jobs answer, and can be asked to run ===")

    code, body = call("GET", "/api/v1/lane")
    expect(
        "the lane status route is registered",
        code,
        200,
        body,
        ("answers with an object", lambda b: isinstance(b, dict)),
        # `lane: false` is a legitimate answer (a process with no lane), so the
        # key has to be there either way; its absence means a different route
        # answered.
        ("says whether this process has a lane at all", lambda b: "lane" in b),
        ("carries a job list", lambda b: isinstance(b.get("jobs"), list)),
    )

    jobs = body.get("jobs", []) if isinstance(body, dict) else []
    # The history beside the instant. `blocked_by` cannot distinguish a job that
    # is eligible and losing every tie-break from one about to run; these can,
    # and their absence is what made that question take a log-file dig.
    check(
        "every job carries its counters, not just its current state",
        all(
            "granted" in j and "lost_to_total" in j and "nudged" in j
            for j in jobs
        ),
        f"{len(jobs)} jobs",
    )
    names = [j.get("job") for j in jobs]
    known = [
        "consolidation",
        "titling",
        "proactive_review",
        "summary_refresh",
        "index_maintenance",
        "memory_extraction",
    ]
    check(
        "every background job is listed, present on this pond or not",
        all(n in names for n in known),
        f"got {names}",
    )
    # A job missing its explanation is the state this whole surface replaces.
    check(
        "every job says what it is waiting for and when it last ran",
        all(
            "blocked_by" in j and "since_last_run_secs" in j and "present" in j
            for j in jobs
        ),
        f"{len(jobs)} jobs",
    )

    # An unknown job must be refused rather than answered cheerfully. A typo
    # that returned 200 would be a button reporting success and doing nothing,
    # which is the exact failure this surface was built to end.
    code, body = call("POST", "/api/v1/lane/jobs/not-a-real-job/run")
    expect(
        "an unknown job is refused",
        code,
        404,
        body,
        ("says which jobs exist", lambda b: isinstance(b.get("known"), list)),
    )

    # And a real one is accepted. `woken` may be either value on this pond --
    # there is no embedding model here, so the extraction loop never spawned --
    # but the route must answer 200 with a verdict rather than 404 or 500.
    code, body = call("POST", "/api/v1/lane/jobs/titling/run")
    expect(
        "a known job can be asked to run now",
        code,
        200,
        body,
        ("names the job it acted on", lambda b: b.get("job") == "titling"),
        ("says whether anything was woken", lambda b: isinstance(b.get("woken"), bool)),
    )

    # `POST /sessions/retitle` used to run up to twenty model calls inside the
    # request handler, holding no lane slot -- so it could decode beside
    # whichever background job already had the machine, and on the Orin it
    # overran the desktop's 30 s client timeout while doing it. It now asks the
    # titling job for its next pass, which is a thing that can be answered in
    # milliseconds.
    #
    # Only a live server can show this: every route test builds an `AppState`
    # with a stub lane, so the real `wake` reaching a real loop is exercised
    # nowhere else.
    import time as _time

    started = _time.monotonic()
    code, body = call("POST", "/api/v1/sessions/retitle")
    elapsed = _time.monotonic() - started
    check(
        "the retitle button answers without decoding anything",
        code in (200, 503) and elapsed < 5.0,
        f"HTTP {code} in {elapsed:.2f}s",
    )
    # The shape, either way. A reply still carrying a count would let the
    # desktop keep reading a number the request can no longer have measured.
    stale = [
        k
        for k in ("renamed", "renamed_count", "considered", "capped", "skipped")
        if isinstance(body, dict) and k in body
    ]
    check(
        "and carries no count it could not have measured",
        not stale,
        f"stale keys: {stale}" if stale else f"{body}",
    )


def section_composed_suggestions():
    """The composed tier: a queue, a settle route, and templates behind it.

    What no unit test can see. The MIGRATION has to have run against a real file
    (0058), the route has to be REGISTERED beside `/suggestions`, and the merge
    has to actually fall back -- this pond has no memories and no model, so every
    suggestion it returns must be a template one, with `composed` present and
    false rather than absent.

    `composed` being on the wire at all is the load-bearing part: it is what the
    client uses to decide whether a tap is worth telling the pond about, and a
    missing field reads as falsy, which silently turns settling off.
    """
    print("\n=== composed suggestions: the queue, and the tier behind it ===")

    code, body = call("GET", "/api/v1/suggestions")
    expect(
        "the suggestions route answers",
        code,
        200,
        body,
        ("carries a list", lambda b: isinstance(b.get("suggestions"), list)),
        # Every suggestion says which tier it came from, template ones included.
        (
            "every suggestion says whether the pond composed it",
            lambda b: all("composed" in s for s in b.get("suggestions", [])),
        ),
        # This pond has no memories and no model, so nothing can have been
        # composed -- and that is the fallback working, not a failure.
        (
            "a pond with nothing to compose from still gets the template tier",
            lambda b: all(s.get("composed") is False for s in b.get("suggestions", [])),
        ),
    )

    con = db()
    tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    check(
        "migration 0058 created the suggestion queue",
        "suggestion_queue" in tables,
        "tables: %s" % sorted(t for t in tables if "sugg" in t),
    )
    # The partial unique index IS the deduplication -- without it a pass that
    # runs every idle period queues a hundred variations of one note.
    indexes = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='index'")}
    check(
        "one live suggestion per memory is enforced by the database",
        "suggestion_queue_one_live_per_memory" in indexes,
        "indexes: %s" % sorted(i for i in indexes if "sugg" in i),
    )

    # Settling an id that was never queued is a 200 saying it changed nothing,
    # not a 404: a double tap on a touch panel is a household being quick.
    code, body = call("POST", "/api/v1/suggestions/never-queued/taken")
    expect(
        "settling an unknown suggestion answers rather than failing",
        code,
        200,
        body,
        ("says it changed nothing", lambda b: b.get("settled") is False),
    )


def section_reminders_surface():
    """A stored reminder can be read and dismissed over real HTTP.

    The table landing was only half of keeping the date. A row nothing can reach
    is a quieter way of losing it than not writing it, and the route that
    reaches it is registered in `routes.rs` beside `/memories/{id}` -- the exact
    neighbourhood where `extraction-status` was once matched as an id. Route
    registration is the thing no unit test sees: every Rust test builds the
    router by hand or not at all.

    The row is seeded through SQL rather than by running an extraction pass,
    because this pond has no embedding model and the engine is therefore not
    running here at all. What is under test is the surface, not the producer.
    """
    print("\n=== reminders: the date can be seen and disposed of ===")

    con = db()
    tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    if "reminders" not in tables:
        check("the reminders surface has a table to read", False, "no reminders table")
        con.close()
        return
    con.execute(
        "INSERT OR REPLACE INTO reminders "
        "(id, about, when_said, about_key, session_id, window_id, subject, "
        " profile_id, said_at, captured_at, disposition) "
        "VALUES ('live-r1', 'the dentist', 'next Tuesday', 'the dentist', "
        "        'sess-live-reminder', 'win-live-1', 'LiveTest', NULL, "
        "        strftime('%Y-%m-%dT%H:%M:%SZ','now'), "
        "        strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'pending')"
    )
    con.commit()
    con.close()

    code, body = call("GET", "/api/v1/reminders")
    ok = expect(
        "the reminders route is registered",
        code,
        200,
        body,
        ("answers with an object", lambda b: isinstance(b, dict)),
        (
            "returns the pending reminder",
            lambda b: isinstance(b, dict)
            and any(r.get("id") == "live-r1" for r in b.get("reminders", [])),
        ),
    )
    if ok:
        row = next(r for r in body["reminders"] if r["id"] == "live-r1")
        check(
            "the timing is the words that were said, not a date",
            row.get("when_said") == "next Tuesday" and "due_at" not in row,
            str(row),
        )
        check(
            "a reminder nobody owns is still readable",
            row.get("profile_id") is None,
            "profile_id=%r -- this is the state of every row on a live pond" % row.get("profile_id"),
        )
        check(
            "it says which conversation it came from",
            row.get("session_id") == "sess-live-reminder",
            str(row),
        )

    # The ordering trap, from the other side. `/reminders` must reach its own
    # handler, and a literal segment under `/memories/` must not be read as an
    # id. The second is already asserted in the extraction section; this is the
    # one route added since, in the same neighbourhood.
    code, body = call("GET", "/api/v1/memories/extraction-status")
    check(
        "a literal path segment is still not matched as an id",
        code == 200 and isinstance(body, dict) and "sessions_total" in body,
        "HTTP %s: %s" % (code, body),
    )

    code, body = call("POST", "/api/v1/reminders/live-r1/dismiss")
    check(
        "a reminder can be dismissed",
        code == 200 and isinstance(body, dict) and body.get("disposition") == "dismissed",
        "HTTP %s: %s" % (code, body),
    )

    code, body = call("GET", "/api/v1/reminders")
    check(
        "a dismissed reminder is not offered again",
        code == 200
        and isinstance(body, dict)
        and not any(r.get("id") == "live-r1" for r in body.get("reminders", [])),
        "HTTP %s: %s" % (code, body),
    )

    # The pond must not say it dismissed something it did not. Both of these are
    # the same answer on purpose: an id that never existed and one already
    # decided are both "there is nothing waiting under that id".
    for label, rid in (
        ("one already dismissed", "live-r1"),
        ("one that never existed", "live-r-nobody"),
    ):
        code, body = call("POST", "/api/v1/reminders/%s/dismiss" % rid)
        check(
            "dismissing %s is a 404, not a second success" % label,
            code == 404,
            "HTTP %s: %s" % (code, body),
        )


# A 1x1 red PNG. Decodable by anything; it never reaches an engine here, because every check
# below expects the turn to be refused before anything is saved.
_ONE_PIXEL_PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=="
)
_PICTURE_SESSION = "live-picture-refused"
_VISION_KINDS = {
    "unknown", "not_declared", "not_on_this_device", "absent", "verifying",
    "downloading", "ready", "failed", "blocked",
}


def _picture_rows(sid):
    con = db()
    sessions = con.execute("SELECT COUNT(*) FROM sessions WHERE id = ?", (sid,)).fetchone()[0]
    messages = con.execute(
        "SELECT COUNT(*) FROM session_messages WHERE session_id = ?", (sid,)
    ).fetchone()[0]
    con.close()
    return sessions, messages


def section_picture_support():
    """Picture support, over real HTTP: the readiness route, and a refused picture turn.

    A picture sent to a model that cannot look at one is refused with a 409 BEFORE anything is
    saved -- no session row, no message row -- because a refused turn that is persisted anyway
    shows up in history as an unanswered photo and is replayed into the next turn. The route
    tests build the router by hand; this is the first place the gate meets the real adapter and
    the real database file, and the restart pass asks the same file again.
    """
    print("\n=== picture support ===")
    code, body = call("GET", "/api/v1/models/vision-status")
    expect(
        "GET /models/vision-status answers",
        code, 200, body,
        ("has model, state, size_bytes, message",
         lambda b: isinstance(b, dict) and {"model", "state", "size_bytes", "message"} <= set(b)),
        ("state is a known kind",
         lambda b: isinstance(b.get("state"), dict) and b["state"].get("kind") in _VISION_KINDS),
    )

    code, before = call("GET", "/api/v1/settings")
    if not check("settings readable before the picture checks", code == 200, str(code)):
        return
    # A local provider with a text-only model. No model file is needed: the gate asks whether the
    # model declares picture support, which is decided by name before any file is read.
    code, body = call(
        "PUT", "/api/v1/settings",
        {"chat_provider": "local", "chat_model": "Llama-3.2-3B-Instruct-Q4_K_M"},
    )
    expect("switch to a text-only local model", code, 200, body)

    code, body = call(
        "POST", "/api/v1/chat/stream",
        {
            "session_id": _PICTURE_SESSION,
            "message": "what is in this photo?",
            "images": [{"data": _ONE_PIXEL_PNG, "mime_type": "image/png"}],
        },
    )
    expect(
        "a picture to a text-only local model is refused",
        code, 409, body,
        ("code is vision_unsupported", lambda b: isinstance(b, dict) and b.get("code") == "vision_unsupported"),
        ("the refusal says what to do",
         lambda b: isinstance(b, dict) and "Models page" in str(b.get("error", ""))),
    )
    sessions, messages = _picture_rows(_PICTURE_SESSION)
    check("the refused picture turn saved no session", sessions == 0, "sessions=%d" % sessions)
    check("the refused picture turn saved no message", messages == 0, "messages=%d" % messages)

    # Put back what the rest of the suite runs on, so later sections see the pond they expect.
    call(
        "PUT", "/api/v1/settings",
        {"chat_provider": before.get("chat_provider", ""), "chat_model": before.get("chat_model", "mock")},
    )


def section_picture_support_after_restart():
    """The refused picture turn is still absent from the file a second server reads."""
    print("\n=== picture support after a restart ===")
    sessions, messages = _picture_rows(_PICTURE_SESSION)
    check("after a restart, the refused picture turn has no session", sessions == 0, "sessions=%d" % sessions)
    check("after a restart, the refused picture turn has no message", messages == 0, "messages=%d" % messages)
    code, body = call("GET", "/api/v1/models/vision-status")
    expect(
        "after a restart, /models/vision-status answers",
        code, 200, body,
        ("state is a known kind",
         lambda b: isinstance(b, dict) and isinstance(b.get("state"), dict)
         and b["state"].get("kind") in _VISION_KINDS),
    )


def main():
    """Auth is NOT checked here.

    scripts/live-test.sh starts this server with POND_DEV_ALLOW_LOOPBACK so the
    functional routes are reachable. Every auth assertion would therefore pass
    regardless of what the allowlist does. The script runs a second server
    without the bypass for that section -- a check that passes because the
    bypass is on reports the opposite of the truth.

    Invoked with the argument `restart`, this runs only the sections that mean
    something on a SECOND server against the same data directory. The identity
    and deletion sections are first-pass only: they create profiles by name and
    would collide with the rows they left behind.
    """
    if len(sys.argv) > 1 and sys.argv[1] == "restart":
        section_secret_store_after_restart()
        section_network_mode_after_restart()
        section_lane_clock_after_restart()
        section_picture_support_after_restart()
    else:
        section_schema()
        jerry, liz = section_identity()
        section_deletion(jerry, liz)
        section_legacy_rows(jerry)
        section_secret_store()
        section_redaction()
        section_network_mode()
        # First pass only. It creates a profile by name, which would collide
        # with the row it left behind, and the process counters it reads are
        # zero on a fresh process by design -- so on the restart pass it would
        # assert nothing the first pass has not already asserted better.
        section_policy_telemetry()
        section_memory_extraction()
        section_inference_lane()
        section_composed_suggestions()
        section_reminders_surface()
        # Last: it moves the chat model to a local one for a moment, and puts it back.
        section_picture_support()

    failed = [label for label, ok, _ in results if not ok]
    print("\n%d checks run, %d failed" % (len(results), len(failed)))
    for f in failed:
        print("  FAILED:", f)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
