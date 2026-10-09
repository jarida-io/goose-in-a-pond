//! Rides as the pond sees them, whatever the provider.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A point a ride starts or ends at. Providers need coordinates; `name` is what the member sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// An upfront fare. Requesting a ride needs its `fare_id`, which the provider expires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FareQuote {
    pub fare_id: String,
    /// The provider's own rendering, e.g. `KES 1,250`; never recomputed here.
    pub display: String,
    pub currency_code: String,
    pub expires_at: DateTime<Utc>,
    /// Minutes until a driver could reach the pickup, when the provider says.
    pub pickup_eta_mins: Option<u32>,
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RideStatus {
    Processing,
    NoDriversAvailable,
    Accepted,
    Arriving,
    InProgress,
    DriverCanceled,
    RiderCanceled,
    Completed,
    /// A status this build doesn't know; kept verbatim, never treated as terminal.
    Other(String),
}

impl RideStatus {
    /// The ride will not change again; tracking stops.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::NoDriversAvailable | Self::DriverCanceled | Self::RiderCanceled | Self::Completed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Driver {
    pub name: String,
    pub phone_number: Option<String>,
    pub rating: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vehicle {
    pub make: String,
    pub model: String,
    pub license_plate: String,
}

/// A requested ride, as last read from the provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ride {
    pub request_id: String,
    pub status: RideStatus,
    pub driver: Option<Driver>,
    pub vehicle: Option<Vehicle>,
    /// Minutes until pickup, while the driver is on the way.
    pub pickup_eta_mins: Option<u32>,
}

impl Ride {
    /// What to tell the member when the ride moves from `previous` to this; `None` when the
    /// change isn't worth a notification.
    pub fn announcement(&self, previous: Option<&RideStatus>) -> Option<String> {
        if previous == Some(&self.status) {
            return None;
        }
        let car = self
            .vehicle
            .as_ref()
            .map(|v| format!("{} {} {}", v.make, v.model, v.license_plate));
        let driver = self.driver.as_ref().map(|d| d.name.as_str());
        Some(match &self.status {
            RideStatus::Processing => return None,
            RideStatus::Accepted => match (driver, car, self.pickup_eta_mins) {
                (Some(d), Some(c), Some(m)) => format!("{d} is coming in a {c}, {m} min away."),
                (Some(d), Some(c), None) => format!("{d} is coming in a {c}."),
                _ => "A driver accepted your ride.".to_string(),
            },
            RideStatus::Arriving => match car {
                Some(c) => format!("Your driver is arriving: {c}."),
                None => "Your driver is arriving.".to_string(),
            },
            RideStatus::InProgress => "Your trip has started.".to_string(),
            RideStatus::Completed => "You have arrived.".to_string(),
            RideStatus::NoDriversAvailable => {
                "No drivers were available. Nothing was charged by this request.".to_string()
            }
            RideStatus::DriverCanceled => "The driver cancelled the ride.".to_string(),
            RideStatus::RiderCanceled => "The ride was cancelled.".to_string(),
            RideStatus::Other(_) => return None,
        })
    }
}

/// Why a ride request returned no ride.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RequestFailure {
    /// The provider refused it, or it was never sent: the provider holds no ride from it.
    #[error("{0}")]
    Refused(String),
    /// No clear answer (a timeout, a dropped connection, a server error): the provider may hold
    /// the ride.
    #[error("{0}")]
    Uncertain(String),
}

/// Where a quoted ride stands in the pond. Only `AwaitingConfirmation` can be confirmed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BookingState {
    AwaitingConfirmation,
    /// Confirmed and sent; the outcome isn't known yet. Blocks a second confirm.
    Requesting,
    Requested {
        ride: Ride,
    },
    /// Sent, and the provider's answer was lost, so it may hold the ride. The member's current
    /// trip with the provider settles it; never retried, and never reported as failed.
    OutcomeUnknown {
        reason: String,
    },
    Declined,
    /// The provider refused the request, so it holds no ride from it. Never retried.
    Failed {
        reason: String,
    },
}

/// The trip a member was quoted, at the fare they confirm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotedTrip {
    pub pickup: Place,
    pub dropoff: Place,
    pub quote: FareQuote,
}

/// One member's ride: a fare waiting for their confirmation, or a ride past it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRide {
    /// The pond's id, which the phone confirms by; not the provider's.
    pub id: String,
    pub profile_id: String,
    /// `None` for a trip the pond found under way with the provider rather than quoted here.
    pub trip: Option<QuotedTrip>,
    pub created_at: DateTime<Utc>,
    pub state: BookingState,
}

impl PendingRide {
    /// How a notification about this ride is titled.
    pub fn title(&self) -> String {
        match &self.trip {
            Some(trip) => format!("Your ride to {}", trip.dropoff.name),
            None => "Your ride".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ride(status: RideStatus) -> Ride {
        Ride {
            request_id: "r".to_string(),
            status,
            driver: Some(Driver {
                name: "Amina".to_string(),
                phone_number: None,
                rating: None,
            }),
            vehicle: Some(Vehicle {
                make: "Toyota".to_string(),
                model: "Axio".to_string(),
                license_plate: "KDA 123A".to_string(),
            }),
            pickup_eta_mins: Some(4),
        }
    }

    #[test]
    fn an_accepted_ride_names_the_driver_car_and_wait() {
        assert_eq!(
            ride(RideStatus::Accepted).announcement(Some(&RideStatus::Processing)),
            Some("Amina is coming in a Toyota Axio KDA 123A, 4 min away.".to_string())
        );
    }

    #[test]
    fn an_unchanged_or_unknown_status_announces_nothing() {
        assert_eq!(
            ride(RideStatus::Arriving).announcement(Some(&RideStatus::Arriving)),
            None
        );
        assert_eq!(ride(RideStatus::Processing).announcement(None), None);
        assert_eq!(
            ride(RideStatus::Other("new_state".into())).announcement(None),
            None
        );
    }

    #[test]
    fn only_finished_rides_are_terminal() {
        assert!(RideStatus::Completed.is_terminal());
        assert!(RideStatus::DriverCanceled.is_terminal());
        assert!(!RideStatus::Arriving.is_terminal());
        assert!(!RideStatus::Other("x".into()).is_terminal());
    }
}

/// Whether the tracker still reads a ride.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reading {
    On,
    /// Stopped after too many failed reads; the member has not been told yet.
    GaveUp,
    GaveUpTold,
}

/// A ride as kept across restarts: the ride, and what its member has been told about it, so a
/// restart neither repeats an update nor forgets that reading it had failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedRide {
    pub ride: PendingRide,
    /// The last status the member was told about, or that needed no telling.
    pub announced: Option<RideStatus>,
    /// Reads in a row that failed, or found no trip for a request without a clear answer.
    pub failed_reads: u32,
    pub reading: Reading,
}
