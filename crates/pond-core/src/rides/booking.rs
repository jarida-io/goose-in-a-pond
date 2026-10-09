//! Quote, confirm, request, track and cancel one member's rides. The member who asked confirms
//! on their own phone; nothing is requested before that, and a quote is confirmed at most once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use super::domain::{
    BookingState, PendingRide, Place, QuotedTrip, Reading, RequestFailure, Ride, RideStatus,
    SavedRide,
};
use super::ports::{RideProvider, RideStore};
use crate::user_data::services::nearby::distance_km;

/// Farther than this, a drop-off is a place matched wrongly, not a ride anyone means to take.
pub const MAX_RIDE_KM: f64 = 150.0;

/// Reads in a row that may fail before the pond stops reading a ride and tells its member.
pub const MAX_FAILED_READS: u32 = 40;

/// How long a ride that is over is kept after it was quoted, so the phone can show how it ended.
pub const KEEP_FINISHED: chrono::Duration = chrono::Duration::hours(24);

#[derive(Debug, thiserror::Error)]
pub enum BookingError {
    #[error("no such ride")]
    NotFound,
    /// Confirming, declining, cancelling or reading another member's ride.
    #[error("this ride belongs to another member")]
    NotYours,
    #[error("the fare expired; ask for a new one")]
    Expired,
    #[error("this ride was already {0}")]
    AlreadyDecided(&'static str),
    #[error("the ride was never requested")]
    NotRequested,
    #[error("the drop-off is {0:.0} km from the pickup, too far for a ride booked here")]
    TooFar(f64),
    #[error("{0}")]
    Provider(#[source] anyhow::Error),
}

pub struct RideBooking {
    provider: Arc<dyn RideProvider>,
    rides: Mutex<HashMap<String, Entry>>,
    /// Where rides are kept across restarts; `None` keeps them in memory only.
    store: Option<Arc<dyn RideStore>>,
}

/// A ride and what its member has been told about it.
struct Entry {
    ride: PendingRide,
    /// The last status the member was told about, or that needed no telling. Trails the ride's own
    /// status until an update is delivered, so an update that failed to send is sent again.
    announced: Option<RideStatus>,
    /// Reads in a row that failed, or found no trip for a request without a clear answer.
    failed_reads: u32,
    reading: Reading,
}

impl Entry {
    fn new(ride: PendingRide, announced: Option<RideStatus>) -> Self {
        Self {
            ride,
            announced,
            failed_reads: 0,
            reading: Reading::On,
        }
    }

    fn saved(&self) -> SavedRide {
        SavedRide {
            ride: self.ride.clone(),
            announced: self.announced.clone(),
            failed_reads: self.failed_reads,
            reading: self.reading,
        }
    }

    fn from_saved(saved: SavedRide) -> Self {
        Self {
            ride: saved.ride,
            announced: saved.announced,
            failed_reads: saved.failed_reads,
            reading: saved.reading,
        }
    }

    /// Nothing more will happen to it, and its member has been told all there is.
    fn is_over(&self, now: DateTime<Utc>) -> bool {
        match self.reading {
            Reading::GaveUpTold => return true,
            Reading::GaveUp => return false,
            Reading::On => {}
        }
        match &self.ride.state {
            BookingState::Declined | BookingState::Failed { .. } => true,
            BookingState::AwaitingConfirmation => self
                .ride
                .trip
                .as_ref()
                .is_none_or(|trip| trip.quote.expires_at <= now),
            BookingState::Requested { ride } => {
                ride.status.is_terminal() && self.announced.as_ref() == Some(&ride.status)
            }
            BookingState::Requesting | BookingState::OutcomeUnknown { .. } => false,
        }
    }
}

/// Something about a ride its member has not been told yet.
#[derive(Debug, Clone, PartialEq)]
pub enum RideNews {
    /// Its status changed; `message` is `None` when that is not worth a notification.
    Status {
        status: RideStatus,
        message: Option<String>,
    },
    /// The pond stopped reading it: too many reads in a row failed.
    LostTrack { message: String },
}

impl RideNews {
    /// Names this news for one ride, so sending it again replaces it rather than adding a copy.
    pub fn key(&self) -> String {
        match self {
            Self::Status { status, .. } => format!("{status:?}"),
            Self::LostTrack { .. } => "lost".to_string(),
        }
    }
}

impl RideBooking {
    pub fn new(provider: Arc<dyn RideProvider>) -> Self {
        Self {
            provider,
            rides: Mutex::new(HashMap::new()),
            store: None,
        }
    }

    /// Keep rides in `store`, so a restart loses neither a quote nor a booked ride.
    pub fn with_store(mut self, store: Arc<dyn RideStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Take back the rides kept before a restart. A ride that was being requested when the pond
    /// stopped may or may not have been booked, so it is never requested again: it becomes
    /// [`BookingState::OutcomeUnknown`], and the tracker asks the provider for the member's trip.
    /// Returns how many rides were taken back.
    pub async fn restore(&self, now: DateTime<Utc>) -> usize {
        let Some(store) = &self.store else {
            return 0;
        };
        let saved = match store.load().await {
            Ok(saved) => saved,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not read the kept rides");
                return 0;
            }
        };
        let mut interrupted = Vec::new();
        {
            let mut rides = self.lock();
            for mut kept in saved {
                if kept.ride.state == BookingState::Requesting {
                    kept.ride.state = BookingState::OutcomeUnknown {
                        reason: "the pond stopped while this ride was being requested".to_string(),
                    };
                    interrupted.push(kept.ride.id.clone());
                }
                rides.insert(kept.ride.id.clone(), Entry::from_saved(kept));
            }
        }
        for id in &interrupted {
            self.persist(id).await;
        }
        let restored = self.lock().len();
        self.prune(now).await;
        restored
    }

    /// Keep this ride as it now stands, or forget it when it is gone. A store that fails is
    /// logged and does not fail the member's action: the ride still works in memory.
    async fn persist(&self, id: &str) {
        let Some(store) = &self.store else {
            return;
        };
        let saved = self.lock().get(id).map(Entry::saved);
        let result = match &saved {
            Some(saved) => store.save(saved).await,
            None => store.remove(id).await,
        };
        if let Err(e) = result {
            tracing::warn!(ride = %id, error = %format!("{e:#}"), "could not keep a ride");
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// Ask the provider for a fare and hold it for `profile_id` to confirm. Requests nothing.
    pub async fn quote(
        &self,
        profile_id: &str,
        pickup: Place,
        dropoff: Place,
        now: DateTime<Utc>,
    ) -> Result<PendingRide, BookingError> {
        let km = distance_km(
            (pickup.latitude, pickup.longitude),
            (dropoff.latitude, dropoff.longitude),
        );
        if km > MAX_RIDE_KM {
            return Err(BookingError::TooFar(km));
        }
        let quote = self
            .provider
            .quote(profile_id, &pickup, &dropoff)
            .await
            .map_err(BookingError::Provider)?;
        let pending = PendingRide {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            trip: Some(QuotedTrip {
                pickup,
                dropoff,
                quote,
            }),
            created_at: now,
            state: BookingState::AwaitingConfirmation,
        };
        self.lock()
            .insert(pending.id.clone(), Entry::new(pending.clone(), None));
        self.persist(&pending.id).await;
        Ok(pending)
    }

    /// The member's yes. Requests the ride once; a second confirm of the same quote is refused.
    /// Returns where the ride then stands: requested, or, when the provider's answer was lost and
    /// no trip of the member's shows the ride, [`BookingState::OutcomeUnknown`].
    pub async fn confirm(
        &self,
        id: &str,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<BookingState, BookingError> {
        // Claimed under the lock, so two taps on Confirm cannot both reach the provider.
        let trip = {
            let mut rides = self.lock();
            let pending = owned(&mut rides, id, profile_id)?;
            let trip = match (&pending.state, &pending.trip) {
                (BookingState::AwaitingConfirmation, Some(trip)) => trip.clone(),
                (other, _) => return Err(BookingError::AlreadyDecided(state_word(other))),
            };
            if now >= trip.quote.expires_at {
                return Err(BookingError::Expired);
            }
            pending.state = BookingState::Requesting;
            trip
        };
        // Kept before the provider is asked: after a crash this ride must read as possibly booked.
        self.persist(id).await;

        let outcome = self
            .provider
            .request(profile_id, &trip.pickup, &trip.dropoff, &trip.quote)
            .await;
        let state = match outcome {
            Ok(ride) => BookingState::Requested { ride },
            Err(RequestFailure::Refused(reason)) => {
                self.set_state(
                    id,
                    BookingState::Failed {
                        reason: reason.clone(),
                    },
                );
                self.persist(id).await;
                return Err(BookingError::Provider(anyhow::anyhow!(reason)));
            }
            Err(RequestFailure::Uncertain(reason)) => {
                tracing::warn!(ride = %id, error = %reason, "a ride request got no clear answer");
                match self.live_trip(Some(id), profile_id).await {
                    Ok(Some(ride)) => BookingState::Requested { ride },
                    Ok(None) => BookingState::OutcomeUnknown { reason },
                    Err(e) => {
                        tracing::warn!(ride = %id, error = %format!("{e:#}"), "could not read the member's current trip");
                        BookingState::OutcomeUnknown { reason }
                    }
                }
            }
        };
        self.set_state(id, state.clone());
        self.persist(id).await;
        Ok(state)
    }

    /// Ask the provider again about a ride whose request got no clear answer: the member's live
    /// trip becomes the ride. Until one shows, it stays [`BookingState::OutcomeUnknown`].
    pub async fn recheck(&self, id: &str, profile_id: &str) -> Result<BookingState, BookingError> {
        let state = self.get(id, profile_id)?.state;
        if !matches!(state, BookingState::OutcomeUnknown { .. }) {
            return Ok(state);
        }
        match self
            .live_trip(Some(id), profile_id)
            .await
            .map_err(BookingError::Provider)?
        {
            Some(ride) => {
                let requested = BookingState::Requested { ride };
                self.set_state(id, requested.clone());
                self.persist(id).await;
                Ok(requested)
            }
            None => Ok(state),
        }
    }

    /// The member's trip under way; `None` when there is none, or a ride here other than
    /// `except` already follows it.
    async fn live_trip(
        &self,
        except: Option<&str>,
        profile_id: &str,
    ) -> anyhow::Result<Option<Ride>> {
        let Some(ride) = self.provider.current(profile_id).await? else {
            return Ok(None);
        };
        if ride.status.is_terminal() || followed(&self.lock(), except, &ride.request_id) {
            return Ok(None);
        }
        Ok(Some(ride))
    }

    /// Follow the member's trip under way with the provider: one booked some other way, or one a
    /// pond without a ride store lost in a restart. Returns its id here, or `None` when there is
    /// no trip under way or a ride here already follows it.
    pub async fn take_over_current(
        &self,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, BookingError> {
        let Some(ride) = self
            .live_trip(None, profile_id)
            .await
            .map_err(BookingError::Provider)?
        else {
            return Ok(None);
        };
        // The same id on every restart, so a link in an earlier update still finds it.
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{}:{}", self.provider.name(), ride.request_id).as_bytes(),
        )
        .to_string();
        {
            let mut rides = self.lock();
            if rides.contains_key(&id) || followed(&rides, None, &ride.request_id) {
                return Ok(None);
            }
            // Told already, as far as anyone here knows; only what changes from now is sent.
            let announced = Some(ride.status.clone());
            rides.insert(
                id.clone(),
                Entry::new(
                    PendingRide {
                        id: id.clone(),
                        profile_id: profile_id.to_string(),
                        trip: None,
                        created_at: now,
                        state: BookingState::Requested { ride },
                    },
                    announced,
                ),
            );
        }
        self.persist(&id).await;
        Ok(Some(id))
    }

    pub async fn decline(&self, id: &str, profile_id: &str) -> Result<(), BookingError> {
        {
            let mut rides = self.lock();
            let pending = owned(&mut rides, id, profile_id)?;
            match &pending.state {
                BookingState::AwaitingConfirmation => pending.state = BookingState::Declined,
                other => return Err(BookingError::AlreadyDecided(state_word(other))),
            }
        }
        self.persist(id).await;
        Ok(())
    }

    /// Cancel a requested ride with the provider. The provider may charge a cancellation fee.
    pub async fn cancel(&self, id: &str, profile_id: &str) -> Result<(), BookingError> {
        self.recheck(id, profile_id).await?;
        let request_id = self.requested_id(id, profile_id)?;
        self.provider
            .cancel(profile_id, &request_id)
            .await
            .map_err(BookingError::Provider)
    }

    /// Read the ride from the provider and keep it.
    pub async fn refresh(&self, id: &str, profile_id: &str) -> Result<Ride, BookingError> {
        let request_id = self.requested_id(id, profile_id)?;
        let ride = self
            .provider
            .ride(profile_id, &request_id)
            .await
            .map_err(BookingError::Provider)?;
        self.set_state(id, BookingState::Requested { ride: ride.clone() });
        self.persist(id).await;
        Ok(ride)
    }

    /// Rides the tracker should look at: requested and still under way, finished with news the
    /// member has not had yet, or sent without a clear answer.
    pub fn to_follow(&self) -> Vec<PendingRide> {
        self.lock()
            .values()
            .filter(|e| match e.reading {
                Reading::GaveUpTold => false,
                Reading::GaveUp => true,
                Reading::On => match &e.ride.state {
                    BookingState::Requested { ride } => {
                        !ride.status.is_terminal() || e.announced.as_ref() != Some(&ride.status)
                    }
                    BookingState::OutcomeUnknown { .. } => true,
                    _ => false,
                },
            })
            .map(|e| e.ride.clone())
            .collect()
    }

    /// Whether the tracker still reads this ride.
    pub fn is_read(&self, id: &str) -> bool {
        self.lock()
            .get(id)
            .is_some_and(|e| e.reading == Reading::On)
    }

    /// A read of the ride worked.
    pub async fn read_succeeded(&self, id: &str) {
        let changed = match self.lock().get_mut(id) {
            Some(entry) if entry.failed_reads != 0 => {
                entry.failed_reads = 0;
                true
            }
            _ => false,
        };
        if changed {
            self.persist(id).await;
        }
    }

    /// A read of the ride failed. Returns whether that was one too many: the pond stops reading
    /// it, and its member is to be told.
    pub async fn read_failed(&self, id: &str) -> bool {
        let gave_up = {
            let mut rides = self.lock();
            let Some(entry) = rides.get_mut(id) else {
                return false;
            };
            entry.failed_reads += 1;
            let gave_up = entry.reading == Reading::On && entry.failed_reads >= MAX_FAILED_READS;
            if gave_up {
                entry.reading = Reading::GaveUp;
            }
            gave_up
        };
        self.persist(id).await;
        gave_up
    }

    /// What the member has not been told about this ride yet, if anything.
    pub fn news(&self, id: &str) -> Option<RideNews> {
        let rides = self.lock();
        let entry = rides.get(id)?;
        match entry.reading {
            Reading::GaveUpTold => return None,
            Reading::GaveUp => {
                let message = match &entry.ride.state {
                    BookingState::OutcomeUnknown { .. } => {
                        "The pond could not confirm this booking. The ride app shows whether it \
                         was booked."
                    }
                    _ => {
                        "The pond can no longer check on this ride. The ride app shows where it is."
                    }
                };
                return Some(RideNews::LostTrack {
                    message: message.to_string(),
                });
            }
            Reading::On => {}
        }
        let BookingState::Requested { ride } = &entry.ride.state else {
            return None;
        };
        if entry.announced.as_ref() == Some(&ride.status) {
            return None;
        }
        Some(RideNews::Status {
            status: ride.status.clone(),
            message: ride.announcement(entry.announced.as_ref()),
        })
    }

    /// Record that the member has had this news, so it is not sent again.
    pub async fn told(&self, id: &str, news: &RideNews) {
        if let Some(entry) = self.lock().get_mut(id) {
            match news {
                RideNews::Status { status, .. } => entry.announced = Some(status.clone()),
                RideNews::LostTrack { .. } => entry.reading = Reading::GaveUpTold,
            }
        }
        self.persist(id).await;
    }

    /// Forget each ride that is over once [`KEEP_FINISHED`] has passed since it was quoted.
    /// Returns how many were forgotten.
    pub async fn prune(&self, now: DateTime<Utc>) -> usize {
        let forgotten: Vec<String> = {
            let mut rides = self.lock();
            let gone: Vec<String> = rides
                .iter()
                .filter(|(_, e)| e.ride.created_at + KEEP_FINISHED <= now && e.is_over(now))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &gone {
                rides.remove(id);
            }
            gone
        };
        for id in &forgotten {
            self.persist(id).await;
        }
        forgotten.len()
    }

    pub fn get(&self, id: &str, profile_id: &str) -> Result<PendingRide, BookingError> {
        let mut rides = self.lock();
        owned(&mut rides, id, profile_id).map(|p| p.clone())
    }

    fn requested_id(&self, id: &str, profile_id: &str) -> Result<String, BookingError> {
        let mut rides = self.lock();
        match &owned(&mut rides, id, profile_id)?.state {
            BookingState::Requested { ride } => Ok(ride.request_id.clone()),
            _ => Err(BookingError::NotRequested),
        }
    }

    fn set_state(&self, id: &str, state: BookingState) {
        if let Some(entry) = self.lock().get_mut(id) {
            entry.ride.state = state;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // A poisoned map still holds consistent rows: every write is a single assignment.
        self.rides.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether a ride here other than `except` follows the provider's `request_id`.
fn followed(rides: &HashMap<String, Entry>, except: Option<&str>, request_id: &str) -> bool {
    rides.iter().any(|(id, entry)| {
        Some(id.as_str()) != except
            && matches!(&entry.ride.state, BookingState::Requested { ride }
                if ride.request_id == request_id)
    })
}

fn owned<'a>(
    rides: &'a mut HashMap<String, Entry>,
    id: &str,
    profile_id: &str,
) -> Result<&'a mut PendingRide, BookingError> {
    let entry = rides.get_mut(id).ok_or(BookingError::NotFound)?;
    if entry.ride.profile_id != profile_id {
        return Err(BookingError::NotYours);
    }
    Ok(&mut entry.ride)
}

fn state_word(state: &BookingState) -> &'static str {
    match state {
        BookingState::AwaitingConfirmation => "waiting",
        BookingState::Requesting => "being requested",
        BookingState::Requested { .. } => "requested",
        BookingState::OutcomeUnknown { .. } => "sent without a clear answer",
        BookingState::Declined => "declined",
        BookingState::Failed { .. } => "tried and failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rides::domain::RideStatus;
    use crate::rides::mocks::{MockRideProvider, OnCurrent, OnRequest};
    use chrono::Duration;

    fn place(name: &str) -> Place {
        Place {
            name: name.to_string(),
            latitude: -1.3,
            longitude: 36.8,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-06T08:00:00Z".parse().unwrap()
    }

    async fn quote_for_liz(booking: &RideBooking) -> PendingRide {
        booking
            .quote("liz", place("Home"), place("JKIA"), now())
            .await
            .unwrap()
    }

    async fn quoted(provider: Arc<MockRideProvider>) -> (RideBooking, PendingRide) {
        let booking = RideBooking::new(provider);
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now())
            .await
            .unwrap();
        (booking, pending)
    }

    #[tokio::test]
    async fn a_quote_requests_nothing() {
        let provider = Arc::new(MockRideProvider::new());
        let (_, pending) = quoted(provider.clone()).await;
        assert_eq!(pending.state, BookingState::AwaitingConfirmation);
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn a_drop_off_too_far_from_the_pickup_is_never_quoted() {
        let provider = Arc::new(MockRideProvider::new());
        let booking = RideBooking::new(provider.clone());
        let mombasa = Place {
            name: "Mombasa".to_string(),
            latitude: -4.0435,
            longitude: 39.6682,
        };
        assert!(matches!(
            booking.quote("liz", place("Home"), mombasa, now()).await,
            Err(BookingError::TooFar(km)) if km > MAX_RIDE_KM
        ));
        assert_eq!(
            provider.quotes(),
            0,
            "a refused trip still asked for a fare"
        );
    }

    #[tokio::test]
    async fn the_member_who_asked_confirms_and_the_ride_is_requested_once() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;

        let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(
            matches!(&state, BookingState::Requested { ride } if ride.status == RideStatus::Processing),
            "{state:?}"
        );

        let again = booking.confirm(&pending.id, "liz", now()).await;
        assert!(matches!(
            again,
            Err(BookingError::AlreadyDecided("requested"))
        ));
        assert_eq!(provider.requests(), 1);
    }

    /// Two taps on Confirm while the first is still with the provider.
    #[tokio::test]
    async fn two_confirms_at_once_make_one_request() {
        let provider = Arc::new(
            MockRideProvider::new().with_request_delay(std::time::Duration::from_millis(50)),
        );
        let (booking, pending) = quoted(provider.clone()).await;

        let (first, second) = tokio::join!(
            booking.confirm(&pending.id, "liz", now()),
            booking.confirm(&pending.id, "liz", now()),
        );
        let refused = [&first, &second]
            .iter()
            .filter(|r| matches!(r, Err(BookingError::AlreadyDecided("being requested"))))
            .count();
        assert_eq!(refused, 1, "{first:?} {second:?}");
        assert!(first.is_ok() || second.is_ok());
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn another_member_cannot_confirm_decline_or_cancel() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;

        assert!(matches!(
            booking.confirm(&pending.id, "jerry", now()).await,
            Err(BookingError::NotYours)
        ));
        assert!(matches!(
            booking.decline(&pending.id, "jerry").await,
            Err(BookingError::NotYours)
        ));
        booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(matches!(
            booking.cancel(&pending.id, "jerry").await,
            Err(BookingError::NotYours)
        ));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn an_expired_fare_cannot_be_confirmed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        let late = pending.trip.as_ref().unwrap().quote.expires_at + Duration::seconds(1);
        assert!(matches!(
            booking.confirm(&pending.id, "liz", late).await,
            Err(BookingError::Expired)
        ));
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn a_declined_ride_cannot_then_be_confirmed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.decline(&pending.id, "liz").await.unwrap();
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::AlreadyDecided("declined"))
        ));
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn a_failed_request_is_never_retried() {
        let provider = Arc::new(MockRideProvider::new().failing_requests());
        let (booking, pending) = quoted(provider.clone()).await;
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::Provider(_))
        ));
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::AlreadyDecided("tried and failed"))
        ));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn a_lost_answer_with_the_members_trip_under_way_is_that_ride() {
        let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
        provider.set_current(OnCurrent::TheRide);
        let (booking, pending) = quoted(provider.clone()).await;

        let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(matches!(state, BookingState::Requested { .. }), "{state:?}");
        assert_eq!(booking.to_follow().len(), 1, "the ride is not followed");
        booking.cancel(&pending.id, "liz").await.unwrap();
        assert_eq!(provider.requests(), 1, "a lost answer was requested again");
    }

    #[tokio::test]
    async fn a_lost_answer_with_no_trip_to_read_is_unknown_never_failed() {
        for current in [OnCurrent::NoTrip, OnCurrent::Unreachable] {
            let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
            provider.set_current(current);
            let (booking, pending) = quoted(provider.clone()).await;

            let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
            assert!(
                matches!(state, BookingState::OutcomeUnknown { .. }),
                "{current:?}: {state:?}"
            );
            assert!(matches!(
                booking.confirm(&pending.id, "liz", now()).await,
                Err(BookingError::AlreadyDecided(_))
            ));
            assert_eq!(booking.to_follow().len(), 1, "{current:?}: not rechecked");

            // The trip shows up later: the recheck takes it, and then it can be cancelled.
            provider.set_current(OnCurrent::TheRide);
            assert!(matches!(
                booking.recheck(&pending.id, "liz").await.unwrap(),
                BookingState::Requested { .. }
            ));
            booking.cancel(&pending.id, "liz").await.unwrap();
            assert_eq!(provider.requests(), 1);
        }
    }

    #[tokio::test]
    async fn a_trip_another_ride_here_already_follows_is_not_taken_twice() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, first) = quoted(provider.clone()).await;
        booking.confirm(&first.id, "liz", now()).await.unwrap();
        let second = booking
            .quote("liz", place("Home"), place("JKIA"), now())
            .await
            .unwrap();

        provider.set_current(OnCurrent::TheRide);
        assert!(
            booking
                .live_trip(Some(&second.id), "liz")
                .await
                .unwrap()
                .is_none(),
            "the first ride's trip was taken as the second's"
        );
        assert!(booking
            .live_trip(Some(&first.id), "liz")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_trip_under_way_at_startup_is_followed_once() {
        let provider = Arc::new(MockRideProvider::new());
        let booking = RideBooking::new(provider.clone());
        assert_eq!(booking.take_over_current("liz", now()).await.unwrap(), None);

        provider.set_current(OnCurrent::TheRide);
        provider.set_status(RideStatus::Accepted);
        let id = booking
            .take_over_current("liz", now())
            .await
            .unwrap()
            .expect("the trip under way is taken over");
        assert_eq!(
            booking.take_over_current("liz", now()).await.unwrap(),
            None,
            "the same trip was taken over twice"
        );
        let followed = booking.to_follow();
        assert_eq!(followed.len(), 1);
        assert_eq!(followed[0].id, id);
        assert_eq!(followed[0].title(), "Your ride");
        assert_eq!(
            booking.news(&id),
            None,
            "a status from before the restart was announced again"
        );

        provider.set_status(RideStatus::Arriving);
        booking.refresh(&id, "liz").await.unwrap();
        assert!(booking.news(&id).is_some());
        assert!(matches!(
            booking.cancel(&id, "jerry").await,
            Err(BookingError::NotYours)
        ));
        booking.cancel(&id, "liz").await.unwrap();
    }

    #[tokio::test]
    async fn a_finished_trip_or_one_already_followed_is_not_taken_over() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();
        provider.set_current(OnCurrent::TheRide);
        assert_eq!(booking.take_over_current("liz", now()).await.unwrap(), None);

        let fresh = RideBooking::new(provider.clone());
        provider.set_status(RideStatus::Completed);
        assert_eq!(fresh.take_over_current("liz", now()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn news_stays_until_the_member_is_told_and_then_stops() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();

        provider.set_status(RideStatus::Accepted);
        booking.refresh(&pending.id, "liz").await.unwrap();
        let news = booking.news(&pending.id).expect("a new status is news");
        assert!(matches!(
            &news,
            RideNews::Status {
                message: Some(_),
                ..
            }
        ));
        assert_eq!(
            booking.news(&pending.id),
            Some(news.clone()),
            "news the member was never told about vanished"
        );
        booking.told(&pending.id, &news).await;
        booking.refresh(&pending.id, "liz").await.unwrap();
        assert_eq!(
            booking.news(&pending.id),
            None,
            "an unchanged status is news again"
        );

        provider.set_status(RideStatus::Completed);
        booking.refresh(&pending.id, "liz").await.unwrap();
        assert_eq!(
            booking.to_follow().len(),
            1,
            "an untold arrival was dropped"
        );
        let arrival = booking.news(&pending.id).unwrap();
        booking.told(&pending.id, &arrival).await;
        assert!(
            booking.to_follow().is_empty(),
            "a finished ride the member was told about is still followed"
        );
    }

    #[tokio::test]
    async fn after_too_many_failed_reads_the_ride_is_no_longer_read_and_the_member_is_told() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();

        for _ in 1..MAX_FAILED_READS {
            assert!(!booking.read_failed(&pending.id).await);
        }
        booking.read_succeeded(&pending.id).await;
        for _ in 1..MAX_FAILED_READS {
            assert!(
                !booking.read_failed(&pending.id).await,
                "a success did not reset the count"
            );
        }
        assert!(booking.read_failed(&pending.id).await);
        assert!(!booking.is_read(&pending.id));
        let news = booking.news(&pending.id).expect("the member is told");
        assert!(matches!(news, RideNews::LostTrack { .. }));
        booking.told(&pending.id, &news).await;
        assert!(booking.to_follow().is_empty());
        assert_eq!(booking.news(&pending.id), None);
    }

    #[tokio::test]
    async fn only_rides_that_are_over_are_forgotten_and_only_after_a_day() {
        let provider = Arc::new(MockRideProvider::new());
        let booking = RideBooking::new(provider.clone());
        let declined = quote_for_liz(&booking).await;
        booking.decline(&declined.id, "liz").await.unwrap();
        let under_way = quote_for_liz(&booking).await;
        booking.confirm(&under_way.id, "liz", now()).await.unwrap();
        let waiting = quote_for_liz(&booking).await;

        assert_eq!(
            booking.prune(now() + Duration::hours(1)).await,
            0,
            "pruned too soon"
        );
        let later = now() + KEEP_FINISHED + Duration::seconds(1);
        // The mock's fare lasts for years, so the waiting quote is not over either.
        assert_eq!(booking.prune(later).await, 1);
        assert!(matches!(
            booking.get(&declined.id, "liz"),
            Err(BookingError::NotFound)
        ));
        assert!(
            booking.get(&under_way.id, "liz").is_ok(),
            "a ride under way was forgotten"
        );
        assert!(booking.get(&waiting.id, "liz").is_ok());

        provider.set_status(RideStatus::Completed);
        booking.refresh(&under_way.id, "liz").await.unwrap();
        assert_eq!(
            booking.prune(later).await,
            0,
            "an arrival nobody was told of was forgotten"
        );
        let arrival = booking.news(&under_way.id).unwrap();
        booking.told(&under_way.id, &arrival).await;
        assert_eq!(booking.prune(later).await, 1);
    }

    #[tokio::test]
    async fn an_unrequested_ride_cannot_be_cancelled_or_refreshed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider).await;
        assert!(matches!(
            booking.cancel(&pending.id, "liz").await,
            Err(BookingError::NotRequested)
        ));
        assert!(matches!(
            booking.refresh(&pending.id, "liz").await,
            Err(BookingError::NotRequested)
        ));
    }
}

#[cfg(test)]
mod store_tests {
    //! Rides kept across a restart: a fresh `RideBooking` over the same store stands in for one.

    use super::*;
    use crate::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use crate::rides::mocks::{MemoryRideStore, MockRideProvider, OnCurrent};

    fn place(name: &str, latitude: f64) -> Place {
        Place {
            name: name.to_string(),
            latitude,
            longitude: 36.8,
        }
    }

    fn booking(provider: &Arc<MockRideProvider>, store: &Arc<MemoryRideStore>) -> RideBooking {
        RideBooking::new(provider.clone()).with_store(store.clone())
    }

    async fn quoted(b: &RideBooking) -> PendingRide {
        b.quote(
            "liz",
            place("Home", -1.27),
            place("JKIA", -1.32),
            Utc::now(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_quote_survives_a_restart_and_is_confirmed_once() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        let pending = quoted(&booking(&provider, &store)).await;

        let restarted = booking(&provider, &store);
        assert_eq!(restarted.restore(Utc::now()).await, 1);
        let state = restarted
            .confirm(&pending.id, "liz", Utc::now())
            .await
            .unwrap();
        assert!(matches!(state, BookingState::Requested { .. }), "{state:?}");
        assert_eq!(provider.requests(), 1);
        assert!(matches!(
            store.kept(&pending.id).unwrap().ride.state,
            BookingState::Requested { .. }
        ));
    }

    #[tokio::test]
    async fn a_ride_is_kept_as_requesting_before_the_company_is_asked() {
        let provider = Arc::new(
            MockRideProvider::new().with_request_delay(std::time::Duration::from_millis(200)),
        );
        let store = Arc::new(MemoryRideStore::new());
        let b = Arc::new(booking(&provider, &store));
        let pending = quoted(&b).await;

        let confirming = {
            let b = b.clone();
            let id = pending.id.clone();
            tokio::spawn(async move { b.confirm(&id, "liz", Utc::now()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            store.kept(&pending.id).unwrap().ride.state,
            BookingState::Requesting,
            "a crash now would leave no sign the ride may have been booked"
        );
        confirming.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_request_cut_off_by_a_restart_is_never_sent_again() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        let pending = quoted(&booking(&provider, &store)).await;
        // The pond stopped between keeping `Requesting` and hearing back.
        let mut kept = store.kept(&pending.id).unwrap();
        kept.ride.state = BookingState::Requesting;
        crate::rides::ports::RideStore::save(store.as_ref(), &kept)
            .await
            .unwrap();

        let restarted = booking(&provider, &store);
        restarted.restore(Utc::now()).await;
        assert!(matches!(
            restarted.get(&pending.id, "liz").unwrap().state,
            BookingState::OutcomeUnknown { .. }
        ));
        assert!(matches!(
            restarted.confirm(&pending.id, "liz", Utc::now()).await,
            Err(BookingError::AlreadyDecided(_))
        ));
        assert_eq!(
            provider.requests(),
            0,
            "a ride that may be booked was requested again"
        );

        // The member's trip under way settles it, as for any answer that was lost.
        provider.set_current(OnCurrent::TheRide);
        let state = restarted.recheck(&pending.id, "liz").await.unwrap();
        assert!(matches!(state, BookingState::Requested { .. }), "{state:?}");
    }

    #[tokio::test]
    async fn an_update_already_sent_is_not_sent_again_after_a_restart() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        let first = booking(&provider, &store);
        let pending = quoted(&first).await;
        first.confirm(&pending.id, "liz", Utc::now()).await.unwrap();
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Accepted);
        crate::rides::tracking::track_once(&first, &notifier).await;
        let sent = notifier.sent().len();
        assert!(sent >= 1);

        let restarted = booking(&provider, &store);
        restarted.restore(Utc::now()).await;
        crate::rides::tracking::track_once(&restarted, &notifier).await;
        assert_eq!(
            notifier.sent().len(),
            sent,
            "the restart repeated an update"
        );
    }

    #[tokio::test]
    async fn failed_reads_are_kept_across_a_restart() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        let first = booking(&provider, &store);
        let pending = quoted(&first).await;
        first.confirm(&pending.id, "liz", Utc::now()).await.unwrap();
        for _ in 0..3 {
            first.read_failed(&pending.id).await;
        }
        let restarted = booking(&provider, &store);
        restarted.restore(Utc::now()).await;
        assert_eq!(store.kept(&pending.id).unwrap().failed_reads, 3);
        restarted.read_failed(&pending.id).await;
        assert_eq!(store.kept(&pending.id).unwrap().failed_reads, 4);
    }

    #[tokio::test]
    async fn a_forgotten_ride_is_removed_from_the_store() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        let b = booking(&provider, &store);
        let pending = quoted(&b).await;
        b.decline(&pending.id, "liz").await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            b.prune(Utc::now() + KEEP_FINISHED + chrono::Duration::hours(1))
                .await,
            1
        );
        assert!(
            store.is_empty(),
            "a finished ride, with its pickup location, was kept"
        );
    }

    #[tokio::test]
    async fn a_store_that_fails_does_not_fail_the_member() {
        let provider = Arc::new(MockRideProvider::new());
        let store = Arc::new(MemoryRideStore::new());
        store.fail_writes(true);
        let b = booking(&provider, &store);
        let pending = quoted(&b).await;
        let state = b.confirm(&pending.id, "liz", Utc::now()).await.unwrap();
        assert!(matches!(state, BookingState::Requested { .. }));
        assert!(store.is_empty());
    }
}
