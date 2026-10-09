//! Asserts `main.rs` still switches ride booking on, on both entry points that run giap-travel.
//! CI only `cargo check`s pond-server, and an unreached `pub fn` never warns, so a lost call
//! would leave booking, or the push to a member's phone, silently off.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");
const STARTUP: &str = include_str!("../../pond-server/src/ride_booking.rs");

/// The body of `async fn {name}(` in main.rs, up to the next top-level fn.
fn entry_point(name: &str) -> &'static str {
    let start = MAIN
        .find(&format!("\nasync fn {name}("))
        .unwrap_or_else(|| panic!("main.rs has no `async fn {name}(`"));
    let rest = &MAIN[start + 1..];
    let end = ["\nfn ", "\nasync fn ", "\npub fn ", "\npub async fn "]
        .iter()
        .filter_map(|next| rest.find(next))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_server_starts_ride_booking() {
    assert!(
        entry_point("run_server").contains("ride_booking::start("),
        "run_server no longer calls ride_booking::start, so book_ride and the phone's ride routes \
         answer that booking is not set up on every pond"
    );
}

/// The desktop's voice screen runs `pond-server chat --voice`, a process of its own.
#[test]
fn both_entry_points_install_the_member_notifier_and_ride_accounts() {
    for (name, accounts) in [
        ("run_server", "ride_booking::start("),
        ("run_chat", "ride_booking::install_accounts("),
    ] {
        let body = entry_point(name);
        assert!(
            body.contains("init_member_notifier("),
            "{name} installs no member notifier, so giap-travel never reaches a phone there"
        );
        let call = body.find(accounts).unwrap_or_else(|| {
            panic!("{name} does not call {accounts}, so book_ride says booking is not set up there")
        });
        let first_argument = body[call + accounts.len()..]
            .split(',')
            .next()
            .unwrap_or_default();
        assert!(
            first_argument.contains("travel_enabled"),
            "{name} passes `{}` as travel's switch, not the household's ext_travel_enabled; \
             booking must stay off unless the household turned travel on",
            first_argument.trim()
        );
    }
}

#[test]
fn startup_installs_every_piece_that_booking_needs() {
    for piece in [
        "pond_api::rides::install(",
        "init_ride_accounts(",
        "tracking::track_once(",
        // A trip booked outside the pond is followed only through this.
        ".take_over_current(",
        // Without these, a restart loses every quote and repeats updates already sent.
        ".with_store(",
        ".restore(",
    ] {
        assert!(
            STARTUP.contains(piece),
            "ride_booking.rs no longer calls {piece}; booking would be half on"
        );
    }
}

/// The store must reach `start`, or `with_store` and `restore` above never run in the server.
#[test]
fn the_server_keeps_rides_in_its_database() {
    let call = MAIN
        .find("ride_booking::start(")
        .expect("main.rs no longer starts ride booking");
    let args = &MAIN[call..call + MAIN[call..].find(".await").unwrap_or(0)];
    assert!(
        args.contains("SqliteRideStore::new("),
        "main.rs starts ride booking without the rides table, so a restart loses every quote"
    );
}
