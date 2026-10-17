use std::time::Duration;

/// How often a pond re-reads its claim and its network to decide whether to advertise.
pub const ADVERTISING_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Unclaimed (even on Ethernet) so its card can claim it, or offline so it can be given a network.
/// The answer starts or stops advertising only; it never ends an open setup session.
pub fn should_advertise(claimed: bool, has_usable_network: bool) -> bool {
    !claimed || !has_usable_network
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pond_advertises_while_unclaimed_or_without_a_network() {
        assert!(should_advertise(false, false));
        assert!(
            should_advertise(false, true),
            "an unclaimed pond on Ethernet still advertises"
        );
        assert!(should_advertise(true, false));
        assert!(
            !should_advertise(true, true),
            "a claimed pond on a network has nothing to offer"
        );
    }
}
