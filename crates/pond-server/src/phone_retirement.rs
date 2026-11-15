//! Runs the removal of phones that stopped connecting, a few times a day.

use std::time::Duration;

use pond_core::user_data::services::phone_retirement::PhoneRetirement;

/// Not at startup: a pond that was off for a month should hear from its phones first.
const FIRST_AFTER: Duration = Duration::from_secs(30 * 60);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

pub fn spawn(retirement: PhoneRetirement) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_AFTER).await;
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            retirement.pass(chrono::Utc::now()).await;
        }
    });
}
