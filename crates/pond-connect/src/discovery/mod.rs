//! The pond's `_pond._tcp` advertisement for each OS, behind pond-core's `ServiceAdvertiser` port.
//! One responder per host owns UDP 5353: on macOS that is the system's mDNSResponder.

use std::sync::Arc;

use pond_core::network::ports::service_advertiser::ServiceAdvertiser;

#[cfg(target_os = "macos")]
pub mod dns_sd;
pub mod mdns;

/// This host's advertiser: mDNSResponder through dns_sd on macOS, mdns-sd everywhere else.
pub fn platform() -> Arc<dyn ServiceAdvertiser> {
    #[cfg(target_os = "macos")]
    return Arc::new(dns_sd::DnsSdAdvertiser::new());
    #[cfg(not(target_os = "macos"))]
    return Arc::new(mdns::MdnsSdAdvertiser::new());
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_os_gets_the_responder_that_owns_its_mdns_port() {
        #[cfg(target_os = "macos")]
        let expected = super::dns_sd::RESPONDER;
        #[cfg(not(target_os = "macos"))]
        let expected = super::mdns::RESPONDER;
        assert_eq!(super::platform().responder(), expected);
    }
}
