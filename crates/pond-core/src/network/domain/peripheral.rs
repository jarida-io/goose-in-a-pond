//! The setup channel's BLE peripheral as the household sees it.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeripheralStatus {
    /// Not started yet.
    Off,
    /// The GATT service is registered and connections are served; nothing is advertised.
    Serving,
    Advertising,
    /// Says why, so the dashboard and `doctor` can tell the household (no radio, no permission).
    Unavailable {
        reason: String,
    },
}

impl PeripheralStatus {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    /// The wire name `GET /network/status` reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Serving => "serving",
            Self::Advertising => "advertising",
            Self::Unavailable { .. } => "unavailable",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable { reason } => Some(reason),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unavailable_peripheral_carries_a_reason() {
        assert_eq!(PeripheralStatus::Advertising.as_str(), "advertising");
        assert_eq!(PeripheralStatus::Serving.reason(), None);
        let off = PeripheralStatus::unavailable("Bluetooth is off");
        assert_eq!(off.as_str(), "unavailable");
        assert_eq!(off.reason(), Some("Bluetooth is off"));
    }
}
