//! Runs members' watched links: every few minutes the due ones are checked, a few at a time, and
//! their owners told on their own devices. Also installs the cart for the `/me/cart` routes and
//! the chat's `web_watch` tool, which share this reader and notifier.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use pond_core::mcp::ports::notification::{
    MemberDelivery, MemberNotifier, Notification, OpenScreen,
};
use pond_core::shared::ports::cart::{CartNotice, CartNotifier};
use pond_core::shared::services::cart_watch::{self, CartWatch};
use pond_core::shared::services::spoken_watch_alerts::SpokenWatchAlerts;
use pond_infra::sqlite_cart::SqliteCartRepository;
use pond_infra::sqlite_hook_event_sink::SqliteHookEventSink;
use pond_mcp_server::product_page::{ProductPageReader, SharedProvider};

/// How often due links are looked for. A member's interval is hours; this bounds how late a check
/// runs.
const PASS_EVERY: Duration = Duration::from_secs(5 * 60);
/// The first pass waits for startup's own work, and the web watches' first pass, to settle.
const FIRST_PASS_AFTER: Duration = Duration::from_secs(150);

/// Tells a link's owner on their own devices, and says a drop aloud to an owner who asked to hear
/// their watches' matches.
pub struct MemberCartNotifier {
    members: Arc<dyn MemberNotifier>,
    speech: Option<Arc<SpokenWatchAlerts>>,
}

impl MemberCartNotifier {
    pub fn new(members: Arc<dyn MemberNotifier>, speech: Option<Arc<SpokenWatchAlerts>>) -> Self {
        Self { members, speech }
    }
}

#[async_trait]
impl CartNotifier for MemberCartNotifier {
    async fn notify(&self, notice: CartNotice) -> anyhow::Result<()> {
        let now = Utc::now();
        // Spawned: speech waits its turn for the speaker, and the checks must not wait with it.
        if let (true, Some(speech)) = (notice.spoken, &self.speech) {
            let (speech, owner) = (speech.clone(), notice.owner.clone());
            let (title, body) = (notice.title.clone(), notice.body.clone());
            tokio::spawn(async move {
                speech.speak(&owner, &title, &body, now).await;
            });
        }
        let notification = Notification {
            id: format!("cart-{}-{}", notice.cart_id, now.timestamp_millis()),
            target: String::new(),
            category: "info".to_string(),
            title: notice.title,
            body: notice.body,
            timestamp: now.to_rfc3339(),
            data: Some(
                OpenScreen::Cart {
                    cart_id: notice.cart_id,
                }
                .data(),
            ),
        };
        let outcome = match self
            .members
            .notify_member(&notice.owner, notification)
            .await
        {
            MemberDelivery::Reached(devices) => format!("queued for {}", devices.len()),
            MemberDelivery::NoPhone => "no_phone".to_string(),
            MemberDelivery::Failed(_) => "failed".to_string(),
        };
        tracing::info!(target: "giap::trace", kind = "cart_notice", outcome = %outcome);
        Ok(())
    }
}

/// Builds the cart over `pool`, installs it for the routes and the chat tool, and starts its
/// loop. `llm` is the live chat model, asked only for a page that names no price.
pub fn spawn(
    pool: sqlx::SqlitePool,
    llm: SharedProvider,
    members: Arc<dyn MemberNotifier>,
    speech: Option<Arc<SpokenWatchAlerts>>,
) -> tokio::task::JoinHandle<()> {
    let watch = Arc::new(CartWatch::new(
        Arc::new(SqliteCartRepository::new(pool.clone())),
        Arc::new(ProductPageReader::new(llm)),
        Arc::new(MemberCartNotifier::new(members, speech)),
        Arc::new(SqliteHookEventSink::new(pool)),
    ));
    cart_watch::install(watch.clone());
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_PASS_AFTER).await;
        let mut clock = tokio::time::interval(PASS_EVERY);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            clock.tick().await;
            match watch.check_due(Utc::now()).await {
                Ok(0) => {}
                Ok(checked) => tracing::info!(target: "giap::trace", kind = "cart_pass", checked),
                Err(e) => {
                    tracing::warn!(target: "giap::trace", kind = "cart_pass_failed", error = %e)
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;

    #[tokio::test]
    async fn a_notice_reaches_the_owners_devices_and_opens_the_link() {
        let members = Arc::new(MockMemberNotifier::new().with_devices("jerry", &["jerry-phone"]));
        let notifier = MemberCartNotifier::new(members.clone(), None);
        notifier
            .notify(CartNotice {
                owner: "jerry".into(),
                cart_id: "c1".into(),
                title: "Cheaper now: Ramtons kettle".into(),
                body: "KES 1,599, down from KES 1,799.".into(),
                spoken: true,
            })
            .await
            .unwrap();
        let sent = members.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "jerry");
        assert_eq!(
            sent[0].1.data,
            Some(serde_json::json!({"open": "cart", "cart_id": "c1"}))
        );
    }
}
