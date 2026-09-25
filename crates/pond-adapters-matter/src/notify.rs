//! Debounced user-facing Matter [`Notification`]s; `giap::trace` still records every occurrence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::user_data::ports::device_commissioning::matter_bridged_endpoint;
use tokio::sync::{Mutex, RwLock};

/// Per-kind alert suppression, matching `routes.rs`'s pairing-failure window.
const ALERT_WINDOW: Duration = Duration::from_secs(600);

/// How long a requested removal's event counts as ours; generous, as a miss is a false alarm.
const REMOVAL_GRACE: Duration = Duration::from_secs(120);

/// Debounce state shared by every clone of a `MatterNotifier`.
#[derive(Default)]
struct State {
    last_pairing_failure: Option<Instant>,
    last_unreachable: Option<Instant>,
    /// Set by an unreachable alert, so only an announced outage gets an all-clear.
    outage_announced: bool,
    /// Set by `setup_started`, so "finished" is only reported after an announced start.
    setup_announced: bool,
    /// Removals GIAP asked for, whose events aren't reported as devices leaving.
    expected_removals: HashMap<String, Instant>,
    /// Ids already answered for: one expectation covers a hub and its children, each only once.
    satisfied_removals: HashMap<String, Instant>,
}

impl State {
    /// Whether GIAP asked for this removal: the exact id, or a child of an expected hub, whose
    /// prefix match isn't consumed. The dash in `matter-9-` keeps it from matching `matter-90`.
    fn take_expected_removal(&mut self, device_id: &str) -> bool {
        // Already answered for: a second departure of the same device is real news.
        if let Some(at) = self.satisfied_removals.get(device_id) {
            if at.elapsed() < REMOVAL_GRACE {
                return false;
            }
        }

        let covered = self.expected_removals.iter().any(|(expected, at)| {
            at.elapsed() < REMOVAL_GRACE
                && (expected == device_id || device_id.starts_with(&format!("{expected}-")))
        });

        if covered {
            self.satisfied_removals
                .insert(device_id.to_string(), Instant::now());
        }
        covered
    }
}

/// Pushes Matter notifications; clones share debounce state so no outage alerts twice.
#[derive(Clone)]
pub struct MatterNotifier {
    /// `None` until attached, making every method a no-op. Attached late: the Matter runtime is
    /// built before the notification stack exists.
    sender: Arc<RwLock<Option<Arc<dyn NotificationSender>>>>,
    state: Arc<Mutex<State>>,
}

impl Default for MatterNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl MatterNotifier {
    /// A notifier that sends nothing until [`attach`](Self::attach) is called.
    pub fn new() -> Self {
        Self {
            sender: Arc::new(RwLock::new(None)),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// For tests and paths with no user to tell; never attached, so it never sends.
    pub fn disabled() -> Self {
        Self::new()
    }

    /// Start sending; attach before the first `apply` or a first-run install finishes unannounced.
    pub async fn attach(&self, sender: Arc<dyn NotificationSender>) {
        *self.sender.write().await = Some(sender);
    }

    /// A device joined; bridged children are covered by the hub's alert, which gives no count
    /// because their descriptors aren't populated yet when the hub registers.
    pub async fn device_paired(&self, device_id: &str, name: &str, device_type: &str) {
        if matter_bridged_endpoint(device_id).is_some() {
            return;
        }
        let body = if device_type == "bridge" {
            format!(
                "\"{name}\" joined this Pond's Matter network. The devices it provides will \
                 appear as it reports them."
            )
        } else {
            format!("\"{name}\" joined this Pond's Matter network as a {device_type}.")
        };
        self.push("info", "Matter device added".to_string(), body)
            .await;
    }

    pub async fn pairing_failed(&self, reason: &str) {
        {
            let mut state = self.state.lock().await;
            if state
                .last_pairing_failure
                .is_some_and(|at| at.elapsed() < ALERT_WINDOW)
            {
                return;
            }
            state.last_pairing_failure = Some(Instant::now());
        }
        self.push(
            "alert",
            "Matter pairing failed".to_string(),
            reason.to_string(),
        )
        .await;
    }

    /// The controller stopped answering; raised at the first revival, past a normal restart's blip.
    pub async fn controller_unreachable(&self, url: &str) {
        {
            let mut state = self.state.lock().await;
            if state
                .last_unreachable
                .is_some_and(|at| at.elapsed() < ALERT_WINDOW)
            {
                return;
            }
            state.last_unreachable = Some(Instant::now());
            state.outage_announced = true;
        }
        self.push(
            "alert",
            "Matter controller is not responding".to_string(),
            format!(
                "This Pond cannot reach its Matter controller at {url}, so Matter devices \
                 cannot be controlled. It is being restarted."
            ),
        )
        .await;
    }

    /// The connection is back; silent unless an outage was announced.
    pub async fn controller_recovered(&self) {
        {
            let mut state = self.state.lock().await;
            if !state.outage_announced {
                return;
            }
            state.outage_announced = false;
        }
        self.push(
            "info",
            "Matter is working again".to_string(),
            "This Pond reconnected to its Matter controller. Matter devices can be controlled \
             again."
                .to_string(),
        )
        .await;
    }

    /// GIAP is removing `device_id` itself: don't alert on it, or on a hub's children.
    pub async fn expect_removal(&self, device_id: &str) {
        let mut state = self.state.lock().await;
        // Sweep, or an expectation whose event never came would suppress a real alert later.
        state
            .expected_removals
            .retain(|_, at| at.elapsed() < REMOVAL_GRACE);
        state
            .satisfied_removals
            .retain(|_, at| at.elapsed() < REMOVAL_GRACE);
        state
            .expected_removals
            .insert(device_id.to_string(), Instant::now());
    }

    /// A device left; silent if GIAP removed it. The alert uses `name`, never the raw id.
    pub async fn device_dropped(&self, device_id: &str, name: &str) {
        {
            let mut state = self.state.lock().await;
            if state.take_expected_removal(device_id) {
                return; // we asked for this
            }
        }
        self.push(
            "alert",
            "A Matter device left the network".to_string(),
            format!(
                "\"{name}\" is no longer on this Pond's Matter network. If it was not \
                 removed deliberately, it may have been factory reset."
            ),
        )
        .await;
    }

    /// First-run setup started; it takes minutes, with only "Starting..." in the UI otherwise.
    pub async fn setup_started(&self) {
        self.state.lock().await.setup_announced = true;
        self.push(
            "info",
            "Setting up Matter".to_string(),
            "This Pond is installing its Matter controller. This takes a few minutes and only \
             happens once."
                .to_string(),
        )
        .await;
    }

    /// Setup finished; reported only if its start was.
    pub async fn setup_finished(&self) {
        {
            let mut state = self.state.lock().await;
            if !state.setup_announced {
                return;
            }
            state.setup_announced = false;
        }
        self.push(
            "info",
            "Matter is ready".to_string(),
            "This Pond's Matter controller is installed and running. Matter devices can now be \
             added from the Devices tab."
                .to_string(),
        )
        .await;
    }

    async fn push(&self, category: &str, title: String, body: String) {
        let Some(sender) = self.sender.read().await.clone() else {
            return;
        };
        let notification = Notification {
            id: uuid::Uuid::new_v4().to_string(),
            target: "broadcast".to_string(),
            category: category.to_string(),
            title,
            body,
            timestamp: chrono::Utc::now().to_rfc3339(),
            data: None,
        };
        if let Err(e) = sender.broadcast(notification).await {
            // A failed notification must not fail the thing it was reporting on.
            tracing::warn!(error = %e, category, "matter: could not push a notification");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;

    #[derive(Default)]
    struct Recorder {
        sent: std::sync::Mutex<Vec<Notification>>,
    }

    impl Recorder {
        fn titles(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|n| n.title.clone())
                .collect()
        }

        /// The body text, which is where a device is named to the user.
        fn bodies(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|n| n.body.clone())
                .collect()
        }
    }

    #[async_trait]
    impl NotificationSender for Recorder {
        async fn send(&self, notification: Notification) -> Result<()> {
            self.sent.lock().unwrap().push(notification);
            Ok(())
        }
        async fn broadcast(&self, notification: Notification) -> Result<()> {
            self.sent.lock().unwrap().push(notification);
            Ok(())
        }
    }

    async fn notifier() -> (MatterNotifier, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let notifier = MatterNotifier::new();
        notifier.attach(recorder.clone()).await;
        (notifier, recorder)
    }

    #[tokio::test]
    async fn a_flapping_controller_alerts_once_not_once_per_attempt() {
        let (notifier, recorder) = notifier().await;
        for _ in 0..5 {
            notifier
                .controller_unreachable("ws://127.0.0.1:5580/giap")
                .await;
        }
        assert_eq!(recorder.titles().len(), 1);
    }

    #[tokio::test]
    async fn recovery_is_only_announced_to_someone_who_heard_about_the_outage() {
        let (notifier, recorder) = notifier().await;

        // An ordinary reconnect, with no outage announced: says nothing.
        notifier.controller_recovered().await;
        assert!(recorder.titles().is_empty(), "an all-clear for nothing");

        notifier
            .controller_unreachable("ws://127.0.0.1:5580/giap")
            .await;
        notifier.controller_recovered().await;
        assert_eq!(
            recorder.titles(),
            vec![
                "Matter controller is not responding",
                "Matter is working again"
            ]
        );

        // And the all-clear is not repeated on the next reconnect.
        notifier.controller_recovered().await;
        assert_eq!(recorder.titles().len(), 2);
    }

    #[tokio::test]
    async fn a_retry_burst_produces_one_pairing_alert() {
        let (notifier, recorder) = notifier().await;
        for _ in 0..4 {
            notifier.pairing_failed("nothing was in pairing mode").await;
        }
        assert_eq!(recorder.titles().len(), 1);
    }

    #[tokio::test]
    async fn setup_finished_says_nothing_when_setup_never_started() {
        let (notifier, recorder) = notifier().await;
        notifier.setup_finished().await;
        assert!(recorder.titles().is_empty());

        notifier.setup_started().await;
        notifier.setup_finished().await;
        assert_eq!(
            recorder.titles(),
            vec!["Setting up Matter", "Matter is ready"]
        );
    }

    #[tokio::test]
    async fn a_removal_giap_asked_for_is_not_reported_as_a_device_leaving() {
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-18").await;
        notifier.device_dropped("matter-18", "Porch Light").await;
        assert!(
            recorder.titles().is_empty(),
            "alerted on a deliberate removal"
        );

        // A different device leaving at the same time is still news.
        notifier.device_dropped("matter-4", "Hall Sensor").await;
        assert_eq!(recorder.titles(), vec!["A Matter device left the network"]);
    }

    #[tokio::test]
    async fn the_expectation_is_consumed_not_permanent() {
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-18").await;
        notifier.device_dropped("matter-18", "Porch Light").await;
        notifier.device_dropped("matter-18", "Porch Light").await;

        assert_eq!(recorder.titles().len(), 1, "the expectation was permanent");
    }

    #[tokio::test]
    async fn a_hub_arriving_with_a_dozen_devices_is_one_alert() {
        let (notifier, recorder) = notifier().await;

        notifier
            .device_paired("matter-90", "Living Room Hub", "bridge")
            .await;
        for child in ["matter-90-3", "matter-90-4", "matter-90-5"] {
            notifier.device_paired(child, "a bulb", "light").await;
        }

        assert_eq!(recorder.titles(), vec!["Matter device added"]);
        // And it claims no count it can't know yet.
        let body = recorder.bodies().join(" ");
        assert!(body.contains("Living Room Hub"), "{body}");
        assert!(body.contains("as it reports them"), "{body}");
    }

    #[tokio::test]
    async fn removing_a_hub_silences_its_children_too() {
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-90").await;
        notifier
            .device_dropped("matter-90", "Living room hub")
            .await;
        for child in ["matter-90-2", "matter-90-3", "matter-90-11"] {
            notifier.device_dropped(child, "a bulb").await;
        }

        assert!(
            recorder.titles().is_empty(),
            "alerted on a hub removal the user asked for: {:?}",
            recorder.titles()
        );
    }

    #[tokio::test]
    async fn one_hubs_removal_does_not_silence_another() {
        // Guards the trailing dash in the hub-prefix match.
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-9").await;
        notifier
            .device_dropped("matter-90-2", "someone else's bulb")
            .await;

        assert_eq!(
            recorder.titles(),
            vec!["A Matter device left the network"],
            "a different hub's child was silenced"
        );
    }

    #[tokio::test]
    async fn a_departure_names_the_device_not_its_id() {
        let (notifier, recorder) = notifier().await;

        notifier.device_dropped("matter-18", "Porch Light").await;

        let body = recorder.bodies().join(" ");
        assert!(
            body.contains("Porch Light"),
            "did not name the device: {body}"
        );
        assert!(!body.contains("matter-18"), "leaked the id: {body}");
    }

    #[tokio::test]
    async fn pairing_success_is_not_debounced() {
        let (notifier, recorder) = notifier().await;
        notifier
            .device_paired("matter-2", "Hall light", "light")
            .await;
        notifier
            .device_paired("matter-3", "Porch lock", "lock")
            .await;
        assert_eq!(recorder.titles().len(), 2);
    }

    #[tokio::test]
    async fn a_notifier_without_a_sender_is_inert() {
        let notifier = MatterNotifier::disabled();
        notifier
            .device_paired("matter-2", "Hall light", "light")
            .await;
        notifier.controller_unreachable("ws://x").await;
    }
}
