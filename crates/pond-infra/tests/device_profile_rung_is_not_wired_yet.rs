//! Path control left after the "rung not wired yet" guards retired; see `device_rung_wiring.rs`.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");
const ROUTES: &str = include_str!("../../pond-api/src/routes.rs");

#[test]
fn the_files_this_test_reads_are_the_ones_it_thinks_they_are() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs -- and this control caught its own author \
         writing `async fn serve`, which no longer exists, on the first run"
    );
    assert!(
        MAIN.contains("issue_pairing_code"),
        "MAIN no longer issues a pairing code; this guard is reading the wrong file \
         or the startup banner has moved"
    );
    assert!(
        ROUTES.contains("async fn handshake_issue_pairing_code"),
        "ROUTES is not pond-api/src/routes.rs, or the loopback issuance route has moved"
    );
    assert!(
        ROUTES.contains("paired_device_profile"),
        "ROUTES no longer builds ResolutionInputs; the resolver call site has moved \
         and the assertions below would pass vacuously"
    );
}
