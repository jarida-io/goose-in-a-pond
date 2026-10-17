//! The pond reads a fresh pairing code aloud for whichever transport asks. Every transport shares
//! one limiter and one speaker, and no answer from any of them carries the code, so only someone
//! in earshot can use it (`docs/architecture/pond-networking.md`, "The spoken fallback").

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;

use crate::network::domain::claim::RetryAfter;
use crate::network::ports::code_announcer::CodeAnnouncer;
use crate::network::services::spoken_code_limiter::SpokenCodeLimiter;
use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;
use crate::security::ports::handshake::Handshake;
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::settings::SettingsRepository;
use crate::user_data::services::member_attribution::owner_for_new_code;

/// What the decision reads, borrowed from the transport that is asking.
pub struct SpokenCodeDeps<'a> {
    pub settings: &'a dyn SettingsRepository,
    pub profiles: &'a dyn ProfileRepository,
    pub handshake: &'a dyn Handshake,
    pub event_log: Option<&'a dyn EventLog>,
}

/// Who asked, for the log line only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenCodeVia {
    Lan { peer: Option<IpAddr> },
    SetupChannel,
}

/// A code was issued and is being read aloud. It carries no digits on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenCodeIssued {
    /// RFC 3339 in whole seconds, so no run of digits in an answer can pass for a code.
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenCodeRefusal {
    /// The household turned spoken codes off (`spoken_pairing_codes`).
    Disabled,
    Limited {
        retry_after: RetryAfter,
    },
    /// The pond cannot make a sound, so no code was issued.
    AnnouncerUnavailable,
}

impl SpokenCodeRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Disabled => "spoken_codes_disabled",
            Self::Limited { .. } => "spoken_code_limited",
            Self::AnnouncerUnavailable => "announcer_unavailable",
        }
    }

    pub fn retry_after(self) -> Option<RetryAfter> {
        match self {
            Self::Limited { retry_after } => Some(retry_after),
            _ => None,
        }
    }
}

/// One per pond: the limiter is the whole point, so a second instance would double the rate.
pub struct SpokenCodeService {
    limiter: Mutex<SpokenCodeLimiter>,
    announcer: Option<Arc<dyn CodeAnnouncer>>,
}

impl Default for SpokenCodeService {
    /// No announcer: every request is `announcer_unavailable`.
    fn default() -> Self {
        Self::new(None)
    }
}

impl SpokenCodeService {
    pub fn new(announcer: Option<Arc<dyn CodeAnnouncer>>) -> Self {
        Self {
            limiter: Mutex::new(SpokenCodeLimiter::new()),
            announcer,
        }
    }

    /// Whether a request could be read aloud now, for a transport listing what it offers.
    pub async fn can_announce(&self) -> bool {
        match &self.announcer {
            Some(announcer) => announcer.is_available().await,
            None => false,
        }
    }

    /// Order matters: the household's switch, then the limiter (which charges), then the speaker,
    /// and only then is a code issued, since issuing retires the code on any screen. `Err` is an
    /// internal failure, never a refusal.
    pub async fn request(
        &self,
        deps: SpokenCodeDeps<'_>,
        via: SpokenCodeVia,
        now: Instant,
    ) -> anyhow::Result<Result<SpokenCodeIssued, SpokenCodeRefusal>> {
        // Fail closed: a household that turned this off must not hear a code on a bad read.
        let settings = deps
            .settings
            .get()
            .await
            .context("reading the spoken pairing code setting")?;
        if !settings.spoken_pairing_codes {
            return Ok(Err(SpokenCodeRefusal::Disabled));
        }

        if let Err(retry_after) = self.try_acquire(now) {
            return Ok(Err(SpokenCodeRefusal::Limited { retry_after }));
        }

        let announcer = match self.announcer.clone() {
            Some(announcer) if announcer.is_available().await => announcer,
            _ => {
                tracing::warn!(
                    kind = "spoken_code_unavailable",
                    ?via,
                    "a phone asked for a spoken pairing code, but this pond cannot make a sound"
                );
                return Ok(Err(SpokenCodeRefusal::AnnouncerUnavailable));
            }
        };

        // The same default attribution as a code issued on the host: a household of one owns it.
        let member_ids: Vec<String> = match deps.profiles.list().await {
            Ok(profiles) => profiles.into_iter().map(|p| p.id).collect(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not list household members; issuing an unattributed spoken code"
                );
                Vec::new()
            }
        };
        let owner = owner_for_new_code(None, false, &member_ids);
        let issued = deps
            .handshake
            .issue_pairing_code_for(owner.as_deref())
            .await
            .context("issuing a spoken pairing code")?;

        let expires_at = chrono::DateTime::parse_from_rfc3339(&issued.expires_at)
            .map(|at| {
                at.with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .unwrap_or_else(|_| issued.expires_at.clone());
        tracing::info!(
            kind = "spoken_code_issued",
            ?via,
            attributed = owner.is_some(),
            expires_at = %expires_at,
            "reading a pairing code aloud"
        );
        record_spoken_code(deps.event_log, owner.is_some()).await;

        // Detached: a first utterance loads the voice, and a phone should not wait seconds to
        // learn that the pond has started speaking.
        let digits = issued.code;
        tokio::spawn(async move {
            if let Err(error) = announcer.announce_code(&digits).await {
                tracing::warn!(
                    kind = "spoken_code_failed",
                    %error,
                    "the pond could not read the pairing code aloud; ask for another in 30 s"
                );
            }
        });

        Ok(Ok(SpokenCodeIssued { expires_at }))
    }

    /// A poisoned lock is recovered: every write leaves the limiter consistent.
    fn try_acquire(&self, now: Instant) -> Result<(), RetryAfter> {
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_acquire(now)
    }
}

/// The audit trail says a code was read aloud, never what it was.
async fn record_spoken_code(event_log: Option<&dyn EventLog>, attributed: bool) {
    let Some(event_log) = event_log else {
        return;
    };
    let event = Event::new(EventCategory::Auth, "auth.spoken_code_issued")
        .sensitivity(PrivacySensitivity::Sensitive)
        .attr("attributed", attributed);
    if let Err(error) = event_log.append(event).await {
        tracing::warn!(%error, "failed to record the spoken pairing code event");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::network::mocks::mock_code_announcer::MockCodeAnnouncer;
    use crate::network::services::spoken_code_limiter::SPOKEN_CODE_SPACING;
    use crate::security::mocks::mock_event_log::MockEventLog;
    use crate::security::ports::handshake::{HandshakeRequest, HandshakeResponse, PairingCode};
    use crate::user_data::mocks::mock_profile::MockProfileRepository;
    use crate::user_data::mocks::mock_settings::MockSettingsRepository;

    /// Issues `123456` and counts how often it was asked.
    #[derive(Default)]
    struct CountingHandshake {
        issued: Mutex<u32>,
    }

    #[async_trait]
    impl Handshake for CountingHandshake {
        async fn handshake(&self, _: HandshakeRequest) -> anyhow::Result<HandshakeResponse> {
            anyhow::bail!("unused")
        }
        async fn validate_token(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn revoke_device(&self, _: &str) -> anyhow::Result<u64> {
            Ok(0)
        }
        async fn revoke_token(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn issue_pairing_code_for(&self, _: Option<&str>) -> anyhow::Result<PairingCode> {
            *self.issued.lock().unwrap() += 1;
            Ok(PairingCode {
                code: "123456".into(),
                expires_at: "2026-10-17T10:10:00.123456Z".into(),
                profile_id: None,
            })
        }
    }

    struct Pond {
        settings: MockSettingsRepository,
        profiles: MockProfileRepository,
        handshake: CountingHandshake,
        events: MockEventLog,
    }

    impl Pond {
        fn new() -> Self {
            Self {
                settings: MockSettingsRepository::new(),
                profiles: MockProfileRepository::new(),
                handshake: CountingHandshake::default(),
                events: MockEventLog::new(),
            }
        }

        fn deps(&self) -> SpokenCodeDeps<'_> {
            SpokenCodeDeps {
                settings: &self.settings,
                profiles: &self.profiles,
                handshake: &self.handshake,
                event_log: Some(&self.events),
            }
        }

        fn issued(&self) -> u32 {
            *self.handshake.issued.lock().unwrap()
        }
    }

    async fn heard(announcer: &MockCodeAnnouncer) -> Vec<String> {
        for _ in 0..100 {
            let announced = announcer.announced();
            if !announced.is_empty() {
                return announced;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        announcer.announced()
    }

    #[tokio::test]
    async fn a_code_is_read_aloud_and_never_returned() {
        let pond = Pond::new();
        let announcer = Arc::new(MockCodeAnnouncer::new());
        let service = SpokenCodeService::new(Some(announcer.clone()));
        let issued = service
            .request(pond.deps(), SpokenCodeVia::SetupChannel, Instant::now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(issued.expires_at, "2026-10-17T10:10:00Z");
        assert!(!format!("{issued:?}").contains("123456"));
        assert_eq!(heard(&announcer).await, vec!["123456"]);
        assert_eq!(pond.events.all().len(), 1);
    }

    #[tokio::test]
    async fn every_transport_shares_one_limit() {
        let pond = Pond::new();
        let service = SpokenCodeService::new(Some(Arc::new(MockCodeAnnouncer::new())));
        let t0 = Instant::now();
        let lan = SpokenCodeVia::Lan { peer: None };
        assert!(service.request(pond.deps(), lan, t0).await.unwrap().is_ok());
        let refusal = service
            .request(pond.deps(), SpokenCodeVia::SetupChannel, t0)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(refusal.code(), "spoken_code_limited");
        assert_eq!(refusal.retry_after(), Some(RetryAfter(SPOKEN_CODE_SPACING)));
        assert_eq!(pond.issued(), 1);
    }

    #[tokio::test]
    async fn a_household_that_turned_it_off_is_refused_before_anything_is_spent() {
        let pond = Pond::new();
        pond.settings
            .set_key("spoken_pairing_codes", "false".into())
            .await
            .unwrap();
        let service = SpokenCodeService::new(Some(Arc::new(MockCodeAnnouncer::new())));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(
                service
                    .request(pond.deps(), SpokenCodeVia::SetupChannel, t0)
                    .await
                    .unwrap(),
                Err(SpokenCodeRefusal::Disabled)
            );
        }
        assert_eq!(pond.issued(), 0);
        assert!(service.try_acquire(t0).is_ok(), "no slot was taken");
    }

    #[tokio::test]
    async fn a_pond_with_no_speaker_issues_no_code() {
        let pond = Pond::new();
        let service = SpokenCodeService::default();
        assert!(!service.can_announce().await);
        assert_eq!(
            service
                .request(pond.deps(), SpokenCodeVia::SetupChannel, Instant::now())
                .await
                .unwrap(),
            Err(SpokenCodeRefusal::AnnouncerUnavailable)
        );

        let silent = Arc::new(MockCodeAnnouncer::new());
        silent.set_unavailable(true);
        let service = SpokenCodeService::new(Some(silent));
        assert!(!service.can_announce().await);
        assert_eq!(
            service
                .request(pond.deps(), SpokenCodeVia::SetupChannel, Instant::now())
                .await
                .unwrap(),
            Err(SpokenCodeRefusal::AnnouncerUnavailable)
        );
        assert_eq!(pond.issued(), 0);
    }
}
