//! Following booked rides: one pass reads every ride still under way and tells its member what
//! changed. The caller decides how often to run it.

use chrono::Utc;

use super::booking::{RideBooking, RideNews};
use super::domain::{BookingState, PendingRide};
use crate::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};

/// One pass. Returns how many members were sent an update. A ride that cannot be read, or an
/// update that could not be sent, is tried again on the next pass; it never stops the others.
/// Rides that are over are forgotten here too.
pub async fn track_once(booking: &RideBooking, notifier: &dyn MemberNotifier) -> usize {
    booking.prune(Utc::now()).await;
    let mut sent = 0;
    for pending in booking.to_follow() {
        if booking.is_read(&pending.id) {
            match read(booking, &pending).await {
                Read::Done => booking.read_succeeded(&pending.id).await,
                Read::Nothing => {}
                Read::Failed(why) => {
                    tracing::warn!(ride = %pending.id, error = %why, "could not read a ride");
                    if !booking.read_failed(&pending.id).await {
                        continue;
                    }
                }
            }
        }
        let Some(news) = booking.news(&pending.id) else {
            continue;
        };
        let (message, data) = match &news {
            RideNews::Status { message: None, .. } => {
                booking.told(&pending.id, &news).await;
                continue;
            }
            RideNews::Status {
                status,
                message: Some(message),
            } => (
                message.clone(),
                serde_json::json!({
                    "action": "ride_update",
                    "ride_id": pending.id,
                    "status": status,
                }),
            ),
            RideNews::LostTrack { message } => (
                message.clone(),
                serde_json::json!({
                    "action": "ride_update",
                    "ride_id": pending.id,
                    "followed": false,
                }),
            ),
        };
        let notification = Notification {
            // Names no status: a push relay sees the id, and a ride's progress is the member's.
            id: uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("ride-{}-{}", pending.id, news.key()).as_bytes(),
            )
            .to_string(),
            target: pending.profile_id.clone(),
            category: "info".to_string(),
            title: pending.title(),
            body: message,
            timestamp: Utc::now().to_rfc3339(),
            data: Some(data),
        };
        match notifier
            .notify_member(&pending.profile_id, notification)
            .await
        {
            MemberDelivery::Reached(_) => {
                booking.told(&pending.id, &news).await;
                sent += 1;
            }
            // Nobody to tell: trying again would only fail the same way.
            MemberDelivery::NoPhone => {
                tracing::info!(ride = %pending.id, "a ride update has no phone to go to");
                booking.told(&pending.id, &news).await;
            }
            MemberDelivery::Failed(why) => {
                tracing::warn!(ride = %pending.id, error = %why, "a ride update was not sent; trying again next pass");
            }
        }
    }
    sent
}

enum Read {
    Done,
    /// A finished ride with news still to send: there is nothing to read.
    Nothing,
    Failed(String),
}

/// Read one ride from the provider. A request without a clear answer that no trip of the
/// member's shows yet counts as a failed read, so the pond gives up on it in time.
async fn read(booking: &RideBooking, pending: &PendingRide) -> Read {
    match &pending.state {
        BookingState::OutcomeUnknown { .. } => {
            match booking.recheck(&pending.id, &pending.profile_id).await {
                Ok(BookingState::Requested { .. }) => Read::Done,
                Ok(_) => Read::Failed("no trip of the member's shows the ride yet".to_string()),
                Err(e) => Read::Failed(e.to_string()),
            }
        }
        BookingState::Requested { ride } if !ride.status.is_terminal() => {
            match booking.refresh(&pending.id, &pending.profile_id).await {
                Ok(_) => Read::Done,
                Err(e) => Read::Failed(e.to_string()),
            }
        }
        _ => Read::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use crate::rides::booking::MAX_FAILED_READS;
    use crate::rides::domain::{Place, RideStatus};
    use crate::rides::mocks::{MockRideProvider, OnCurrent, OnRequest};
    use std::sync::Arc;

    fn place(name: &str) -> Place {
        Place {
            name: name.to_string(),
            latitude: -1.3,
            longitude: 36.8,
        }
    }

    async fn booked(provider: Arc<MockRideProvider>) -> (RideBooking, String) {
        let booking = RideBooking::new(provider);
        let now = Utc::now();
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now)
            .await
            .unwrap();
        booking.confirm(&pending.id, "liz", now).await.unwrap();
        (booking, pending.id)
    }

    #[tokio::test]
    async fn a_status_change_reaches_the_member_once() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, ride_id) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert_eq!(
            track_once(&booking, &notifier).await,
            0,
            "the same status was sent twice"
        );

        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
        assert_eq!(sent[0].1.title, "Your ride to JKIA");
        assert!(sent[0].1.body.contains("Amina"), "{}", sent[0].1.body);
        let data = sent[0].1.data.as_ref().unwrap();
        assert_eq!(data["action"], "ride_update");
        assert_eq!(data["ride_id"], ride_id);
    }

    #[tokio::test]
    async fn a_finished_ride_stops_being_followed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Completed);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        provider.set_status(RideStatus::Arriving);
        assert_eq!(
            track_once(&booking, &notifier).await,
            0,
            "a completed ride was read again"
        );
    }

    #[tokio::test]
    async fn an_update_that_failed_to_send_is_sent_on_the_next_pass() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new()
            .with_devices("liz", &["liz-phone"])
            .failing_next(1);

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        let sent = notifier.sent();
        assert_eq!(sent.len(), 2, "one failed try, one delivery");
        assert_eq!(
            sent[0].1.body, sent[1].1.body,
            "the retry carries the same news"
        );
        assert_eq!(track_once(&booking, &notifier).await, 0);
    }

    #[tokio::test]
    async fn an_arrival_that_failed_to_send_is_retried_without_reading_the_ride_again() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new()
            .with_devices("liz", &["liz-phone"])
            .failing_next(1);

        provider.set_status(RideStatus::Completed);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        // A finished ride is not read again, so this later status is never seen.
        provider.set_status(RideStatus::Arriving);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert_eq!(notifier.sent()[1].1.body, "You have arrived.");
        assert!(booking.to_follow().is_empty());
    }

    #[tokio::test]
    async fn a_member_with_no_phone_is_not_tried_again_and_again() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new();

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(notifier.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_ride_whose_answer_was_lost_is_found_and_followed() {
        let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
        provider.set_current(OnCurrent::Unreachable);
        let booking = RideBooking::new(provider.clone());
        let now = Utc::now();
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now)
            .await
            .unwrap();
        booking.confirm(&pending.id, "liz", now).await.unwrap();
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        assert_eq!(track_once(&booking, &notifier).await, 0);
        provider.set_current(OnCurrent::TheRide);
        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert!(notifier.sent()[0].1.body.contains("Amina"));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn a_ride_created_with_no_drivers_available_is_still_announced() {
        let provider = Arc::new(MockRideProvider::new());
        provider.set_status(RideStatus::NoDriversAvailable);
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert!(notifier.sent()[0].1.body.contains("No drivers"));
        assert_eq!(track_once(&booking, &notifier).await, 0);
    }

    #[tokio::test]
    async fn the_notification_id_does_not_name_the_status() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, ride_id) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Accepted);
        track_once(&booking, &notifier).await;
        let id = notifier.sent()[0].1.id.clone();
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "{id}");
        assert!(!id.contains("Accepted") && !id.contains(&ride_id), "{id}");
        let expected = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("ride-{ride_id}-Accepted").as_bytes(),
        );
        assert_eq!(id, expected.to_string(), "a re-send must replace, not add");
    }

    #[tokio::test]
    async fn a_ride_that_cannot_be_read_is_given_up_on_and_its_member_told_once() {
        let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
        provider.set_current(OnCurrent::Unreachable);
        let booking = RideBooking::new(provider.clone());
        let now = Utc::now();
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now)
            .await
            .unwrap();
        booking.confirm(&pending.id, "liz", now).await.unwrap();
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        for _ in 1..MAX_FAILED_READS {
            assert_eq!(track_once(&booking, &notifier).await, 0);
        }
        assert_eq!(track_once(&booking, &notifier).await, 1);
        let told = &notifier.sent()[0].1;
        assert!(told.body.contains("could not confirm"), "{}", told.body);
        assert_eq!(told.data.as_ref().unwrap()["followed"], false);

        // No more reads, and nothing more to say, even once the trip shows up.
        provider.set_current(OnCurrent::TheRide);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(notifier.sent().len(), 1);
        assert!(booking.to_follow().is_empty());
    }

    #[tokio::test]
    async fn nothing_is_sent_for_a_ride_still_being_matched() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert!(notifier.sent().is_empty());
    }
}
