-- Rides, kept across restarts.
--
-- ── What this replaces ───────────────────────────────────────────────────────
--
-- `RideBooking` kept rides in a `HashMap` only. A restart lost every fare
-- quoted but not yet confirmed, so a member who tapped Book this ride after one
-- was told there was no such ride; and it lost what each member had been told,
-- so a ride taken back over from Uber could have its updates repeated.
--
-- ── Why one JSON column ──────────────────────────────────────────────────────
--
-- A row is `SavedRide` (pond-core/src/rides/domain.rs) as JSON: the ride, the
-- last status its member was told, the failed-read count and whether it is
-- still read. All of it is written and read whole, by id, never queried by
-- field, so columns per field would only be a second place to keep in step
-- with the domain. A row that no longer decodes is skipped on load and logged.
--
-- ── Privacy ──────────────────────────────────────────────────────────────────
--
-- A row holds the member's pickup and drop-off, which is where they were and
-- where they went. It is removed when the ride is forgotten (a day after it was
-- quoted, once it is over), and with the member: ON DELETE CASCADE. It holds no
-- token; Uber sign-ins stay in the secret store.

CREATE TABLE IF NOT EXISTS rides (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    saved       TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rides_profile ON rides(profile_id);
