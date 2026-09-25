//! Live Breez/Spark test, `#[ignore]`d; Regtest unless `LIGHTNING_NETWORK=mainnet`. Run with
//! `BREEZ_API_KEY=<key> ... -- --ignored` (key: breez.technology/request-api-key).

use pond_adapters_lightning::{LightningConfig, LightningPaymentRail};
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::ports::payment_rail::PaymentRail;

#[tokio::test]
#[ignore = "requires a real BREEZ_API_KEY (breez.technology/request-api-key)"]
async fn issuing_a_real_invoice_round_trips_through_list_payments() {
    let config = LightningConfig::from_env("/tmp/pond-lightning-live-test".to_string())
        .expect("BREEZ_API_KEY must be set to run this test");
    let (rail, generated_mnemonic) = LightningPaymentRail::connect(config)
        .await
        .expect("failed to connect to Spark network");
    if let Some(mnemonic) = generated_mnemonic {
        // File, not log: a real wallet seed must never reach a terminal or CI log.
        let path = "/tmp/pond-lightning-live-test/generated-mnemonic.txt";
        std::fs::write(path, &mnemonic).expect("failed to save generated mnemonic to disk");
        eprintln!(
            "generated a fresh wallet mnemonic for this test run — not persisted by \
             the test itself, saved to {path} (delete after copying it into \
             SettingsRepository, if this run is meant to be kept)"
        );
    }

    let invoice = rail
        .issue_invoice(Millisats::new(1000))
        .await
        .expect("issue_invoice should succeed against a live connection");
    assert!(
        invoice.starts_with("ln"),
        "expected a bolt11 invoice string, got: {invoice}"
    );

    // Unpaid, so no preimage exists: false or Err are both fine; true is a false positive.
    if let Ok(matched) = rail.verify_preimage(&invoice, "0000").await {
        assert!(!matched, "an unpaid invoice must never verify as paid");
    }
}
