use async_trait::async_trait;

use crate::network::domain::pond_advert::{AdvertiseError, PondAdvert};

/// Publishes the pond's `_pond._tcp` instance through the host's mDNS responder. One responder
/// per host owns UDP 5353: a second one beside it answers nothing reliably (measured on macOS).
#[async_trait]
pub trait ServiceAdvertiser: Send + Sync {
    /// Registers `advert`, or replaces the TXT record in place when its instance and port are the
    /// ones already registered, so a phone that resolved the pond keeps the same name. An error
    /// leaves the pond unadvertised for now; the adapter keeps trying with the last advert.
    async fn publish(&self, advert: &PondAdvert) -> Result<(), AdvertiseError>;

    /// Sends goodbyes and stops answering. Idempotent; a later `publish` registers again.
    async fn withdraw(&self);

    /// Which responder answers for the pond, for the startup banner and the logs.
    fn responder(&self) -> &'static str;
}
