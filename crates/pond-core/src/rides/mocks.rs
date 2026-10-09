//! Test double for [`RideProvider`]: quotes a fixed fare, counts requests, and reports whatever
//! status a test sets.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};

use super::domain::{Driver, FareQuote, Place, RequestFailure, Ride, RideStatus, Vehicle};
use super::ports::RideProvider;

/// What `request` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnRequest {
    /// Books the ride and says so.
    Book,
    /// Refuses it; no ride exists.
    Refuse,
    /// Books the ride, but the answer never arrives.
    LoseTheAnswer,
}

/// What `current` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnCurrent {
    /// The member's trip under way is the ride this mock books.
    TheRide,
    NoTrip,
    /// The read fails.
    Unreachable,
}

pub struct MockRideProvider {
    status: Mutex<RideStatus>,
    quotes: Mutex<usize>,
    /// Requests made, by member.
    requests: Mutex<HashMap<String, usize>>,
    on_request: Mutex<OnRequest>,
    on_current: Mutex<OnCurrent>,
    /// How long `request` takes, so a test can overlap two.
    request_delay: Option<std::time::Duration>,
}

impl Default for MockRideProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRideProvider {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(RideStatus::Processing),
            quotes: Mutex::new(0),
            requests: Mutex::new(HashMap::new()),
            on_request: Mutex::new(OnRequest::Book),
            on_current: Mutex::new(OnCurrent::NoTrip),
            request_delay: None,
        }
    }

    /// Each `request` takes this long before it answers.
    pub fn with_request_delay(mut self, delay: std::time::Duration) -> Self {
        self.request_delay = Some(delay);
        self
    }

    /// Every `request` is refused, as Uber refusing an expired fare would be.
    pub fn failing_requests(self) -> Self {
        self.on_request(OnRequest::Refuse)
    }

    pub fn on_request(self, behaviour: OnRequest) -> Self {
        *self.on_request.lock().unwrap() = behaviour;
        self
    }

    pub fn set_current(&self, answer: OnCurrent) {
        *self.on_current.lock().unwrap() = answer;
    }

    pub fn set_status(&self, status: RideStatus) {
        *self.status.lock().unwrap() = status;
    }

    /// How many times `quote` was called.
    pub fn quotes(&self) -> usize {
        *self.quotes.lock().unwrap()
    }

    /// How many times `request` was called.
    pub fn requests(&self) -> usize {
        self.requests.lock().unwrap().values().sum()
    }

    /// How many times `request` was called for this member.
    pub fn requests_for(&self, profile_id: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .get(profile_id)
            .copied()
            .unwrap_or(0)
    }

    fn ride(&self) -> Ride {
        Ride {
            request_id: "req-1".to_string(),
            status: self.status.lock().unwrap().clone(),
            driver: Some(Driver {
                name: "Amina".to_string(),
                phone_number: None,
                rating: Some(4.9),
            }),
            vehicle: Some(Vehicle {
                make: "Toyota".to_string(),
                model: "Axio".to_string(),
                license_plate: "KDA 123A".to_string(),
            }),
            pickup_eta_mins: Some(4),
        }
    }
}

#[async_trait]
impl RideProvider for MockRideProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn quote(
        &self,
        _profile_id: &str,
        _pickup: &Place,
        _dropoff: &Place,
    ) -> Result<FareQuote> {
        *self.quotes.lock().unwrap() += 1;
        Ok(FareQuote {
            fare_id: "fare-1".to_string(),
            display: "KES 1,250".to_string(),
            currency_code: "KES".to_string(),
            expires_at: Utc::now() + Duration::days(3650),
            pickup_eta_mins: Some(4),
            product_id: None,
        })
    }

    async fn request(
        &self,
        profile_id: &str,
        _pickup: &Place,
        _dropoff: &Place,
        _quote: &FareQuote,
    ) -> std::result::Result<Ride, RequestFailure> {
        *self
            .requests
            .lock()
            .unwrap()
            .entry(profile_id.to_string())
            .or_default() += 1;
        if let Some(delay) = self.request_delay {
            tokio::time::sleep(delay).await;
        }
        match *self.on_request.lock().unwrap() {
            OnRequest::Book => Ok(self.ride()),
            OnRequest::Refuse => Err(RequestFailure::Refused(
                "409 fare_expired: The fare has expired.".to_string(),
            )),
            OnRequest::LoseTheAnswer => Err(RequestFailure::Uncertain(
                "the request timed out".to_string(),
            )),
        }
    }

    async fn ride(&self, _profile_id: &str, _request_id: &str) -> Result<Ride> {
        Ok(self.ride())
    }

    async fn current(&self, _profile_id: &str) -> Result<Option<Ride>> {
        match *self.on_current.lock().unwrap() {
            OnCurrent::TheRide => Ok(Some(self.ride())),
            OnCurrent::NoTrip => Ok(None),
            OnCurrent::Unreachable => Err(anyhow!("provider unavailable")),
        }
    }

    async fn cancel(&self, _profile_id: &str, _request_id: &str) -> Result<()> {
        self.set_status(RideStatus::RiderCanceled);
        Ok(())
    }
}

/// Test double for [`RideStore`](super::ports::RideStore): keeps rides in a map, and can be told
/// to fail every write, as a full disk would.
#[derive(Default)]
pub struct MemoryRideStore {
    rides: Mutex<HashMap<String, super::domain::SavedRide>>,
    failing: std::sync::atomic::AtomicBool,
}

impl MemoryRideStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_writes(&self, failing: bool) {
        self.failing
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    /// The ride as kept, if it is.
    pub fn kept(&self, id: &str) -> Option<super::domain::SavedRide> {
        self.rides.lock().unwrap().get(id).cloned()
    }

    pub fn len(&self) -> usize {
        self.rides.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check(&self) -> Result<()> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("disk full"));
        }
        Ok(())
    }
}

#[async_trait]
impl super::ports::RideStore for MemoryRideStore {
    async fn save(&self, ride: &super::domain::SavedRide) -> Result<()> {
        self.check()?;
        self.rides
            .lock()
            .unwrap()
            .insert(ride.ride.id.clone(), ride.clone());
        Ok(())
    }

    async fn remove(&self, id: &str) -> Result<()> {
        self.check()?;
        self.rides.lock().unwrap().remove(id);
        Ok(())
    }

    async fn load(&self) -> Result<Vec<super::domain::SavedRide>> {
        Ok(self.rides.lock().unwrap().values().cloned().collect())
    }
}
