//! A web watch's match said aloud, for a member who asked for it. It is spoken only while the
//! household allows the pond to speak first, outside the household's quiet hours and the member's
//! own, while presence says the member is home, and while no conversation holds the model.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use futures::StreamExt;

use crate::models::ports::announce::Announce;
use crate::shared::domain::session_activity::ProfilePresence;
use crate::shared::ports::event_bus::{BusEvent, EventBus};
use crate::shared::services::agent_activations::PresenceBoard;
use crate::shared::services::chat::{
    quiet_hours_cover, LocalTimeOfDay, SpeechRefusal, UnpromptedSpeech,
};
use crate::user_data::domain::settings::Settings;
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::settings::SettingsRepository;

/// `Profile.preferences` keys: `"true"` to hear matches aloud, and the member's quiet hours.
pub const SPOKEN_ALERTS_KEY: &str = "web_watch_spoken_alerts";
pub const QUIET_START_KEY: &str = "web_watch_quiet_start";
pub const QUIET_END_KEY: &str = "web_watch_quiet_end";

pub const DEFAULT_QUIET_START: &str = "22:00";
pub const DEFAULT_QUIET_END: &str = "07:00";

/// A member's choice about hearing their watches' matches aloud. Off until they turn it on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpokenAlertChoice {
    pub enabled: bool,
    /// Local `"HH:MM"`; wraps midnight when after `quiet_end`.
    pub quiet_start: String,
    pub quiet_end: String,
}

impl SpokenAlertChoice {
    pub fn of(preferences: &HashMap<String, String>) -> Self {
        let time = |key: &str, default: &str| {
            preferences
                .get(key)
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        Self {
            enabled: preferences
                .get(SPOKEN_ALERTS_KEY)
                .is_some_and(|v| v.trim() == "true"),
            quiet_start: time(QUIET_START_KEY, DEFAULT_QUIET_START),
            quiet_end: time(QUIET_END_KEY, DEFAULT_QUIET_END),
        }
    }

    pub fn write_into(&self, preferences: &mut HashMap<String, String>) {
        preferences.insert(SPOKEN_ALERTS_KEY.into(), self.enabled.to_string());
        preferences.insert(QUIET_START_KEY.into(), self.quiet_start.clone());
        preferences.insert(QUIET_END_KEY.into(), self.quiet_end.clone());
    }
}

/// Whether `time` is a local `"HH:MM"` the quiet-hours check reads.
pub fn is_time_of_day(time: &str) -> bool {
    chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M").is_ok()
}

/// Whether a watch's match may be said aloud to its owner now. Quiet hours come first, as in
/// `decide_unprompted_speech`; the member's own choice stands in for the household's categories.
pub fn decide_spoken_alert(
    settings: Option<&Settings>,
    choice: &SpokenAlertChoice,
    member_home: bool,
    turn_in_flight: bool,
    text: &str,
    now: LocalTimeOfDay,
) -> UnpromptedSpeech {
    use UnpromptedSpeech::Refused;

    let Some(settings) = settings else {
        return Refused(SpeechRefusal::SettingsUnreadable);
    };
    match quiet_hours_cover(&settings.quiet_hours_start, &settings.quiet_hours_end, now) {
        None => return Refused(SpeechRefusal::QuietHoursUnreadable),
        Some(true) => return Refused(SpeechRefusal::QuietHours),
        Some(false) => {}
    }
    // A broken bound of the member's own is read as quiet all day, never as no quiet hours.
    if quiet_hours_cover(&choice.quiet_start, &choice.quiet_end, now) != Some(false) {
        return Refused(SpeechRefusal::MemberQuietHours);
    }
    if !settings.unprompted_speech_enabled {
        return Refused(SpeechRefusal::NotEnabled);
    }
    if !choice.enabled {
        return Refused(SpeechRefusal::NotOptedIn);
    }
    if turn_in_flight {
        return Refused(SpeechRefusal::MidConversation);
    }
    if !member_home {
        return Refused(SpeechRefusal::MemberNotPresent);
    }
    if text.trim().is_empty() {
        return Refused(SpeechRefusal::NothingToSay);
    }
    UnpromptedSpeech::Spoken
}

/// Who presence says is home, kept from the bus's presence events.
pub type SharedPresence = Arc<RwLock<PresenceBoard>>;

/// A board kept current from `bus`'s presence events for as long as the bus lives. It starts
/// empty, so after a restart nobody is home until presence says so again.
pub fn follow_presence(bus: &Arc<dyn EventBus>) -> SharedPresence {
    let board = SharedPresence::default();
    let mut events = bus.subscribe();
    let kept = board.clone();
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let BusEvent::Presence(presence) = event {
                record(&kept, &presence);
            }
        }
    });
    board
}

fn record(board: &SharedPresence, presence: &ProfilePresence) {
    board
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .record(presence);
}

fn is_home(board: &SharedPresence, member: &str) -> bool {
    board
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot()
        .iter()
        .any(|(id, home, _)| id == member && *home)
}

pub struct SpokenWatchAlerts {
    profiles: Arc<dyn ProfileRepository + Send + Sync>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    presence: SharedPresence,
    voice: Arc<dyn Announce>,
    turn_in_flight: fn() -> bool,
}

impl SpokenWatchAlerts {
    pub fn new(
        profiles: Arc<dyn ProfileRepository + Send + Sync>,
        settings: Arc<dyn SettingsRepository + Send + Sync>,
        presence: SharedPresence,
        voice: Arc<dyn Announce>,
    ) -> Self {
        Self {
            profiles,
            settings,
            presence,
            voice,
            turn_in_flight: crate::models::domain::inference_bus::turn_has_the_model,
        }
    }

    #[cfg(test)]
    fn with_turn_check(mut self, turn_in_flight: fn() -> bool) -> Self {
        self.turn_in_flight = turn_in_flight;
        self
    }

    /// Says a match notice to its owner when the gate allows: their name, the notice's title
    /// and its first line. Returns what was decided.
    pub async fn speak(
        &self,
        owner: &str,
        title: &str,
        body: &str,
        now: DateTime<Utc>,
    ) -> UnpromptedSpeech {
        let settings = match self.settings.get().await {
            Ok(settings) => Some(settings),
            Err(e) => {
                tracing::warn!(target: "giap::trace", kind = "spoken_watch_alert_settings", error = %e);
                None
            }
        };
        let profile = self.profiles.get(owner).await.ok().flatten();
        let choice = profile
            .as_ref()
            .map(|p| SpokenAlertChoice::of(&p.preferences))
            .unwrap_or_else(|| SpokenAlertChoice::of(&HashMap::new()));
        let name = profile.as_ref().map(|p| {
            p.preferences
                .get("preferred_name")
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(&p.display_name)
                .trim()
                .to_string()
        });
        let text = spoken_text(name.as_deref(), title, body);
        let local = LocalTimeOfDay::in_zone(
            settings.as_ref().map_or("UTC", |s| s.timezone.as_str()),
            now,
        );
        let decision = decide_spoken_alert(
            settings.as_ref(),
            &choice,
            is_home(&self.presence, owner),
            (self.turn_in_flight)(),
            &text,
            local,
        );
        match &decision {
            UnpromptedSpeech::Spoken => {
                if let Err(e) = self.voice.announce(&text).await {
                    tracing::warn!(target: "giap::trace", kind = "spoken_watch_alert_failed", error = %e);
                }
            }
            UnpromptedSpeech::Refused(why) => {
                tracing::debug!(
                    target: "giap::trace",
                    kind = "spoken_watch_alert_withheld",
                    reason = why.as_str(),
                );
            }
        }
        decision
    }
}

/// "Amina, a match on Used cars: New: 2016 Subaru Forester, 1,400,000."
fn spoken_text(name: Option<&str>, title: &str, body: &str) -> String {
    let first = body.lines().map(str::trim).find(|l| !l.is_empty());
    let mut text = match name.filter(|n| !n.is_empty()) {
        Some(name) => format!("{name}, {}", lower_first(title)),
        None => title.to_string(),
    };
    if let Some(line) = first {
        text.push_str(": ");
        text.push_str(line.trim_end_matches('.'));
    }
    if !text.is_empty() {
        text.push('.');
    }
    text
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::session_activity::PresenceTransition;
    use crate::user_data::domain::profile::CreateProfileRequest;
    use crate::user_data::domain::session::IdentificationSource;
    use crate::user_data::mocks::mock_profile::MockProfileRepository;
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn household(speech: bool) -> Settings {
        Settings {
            unprompted_speech_enabled: speech,
            timezone: "UTC".into(),
            ..Settings::default()
        }
    }

    fn opted_in() -> SpokenAlertChoice {
        SpokenAlertChoice {
            enabled: true,
            ..SpokenAlertChoice::of(&HashMap::new())
        }
    }

    fn at(hour: u32, minute: u32) -> LocalTimeOfDay {
        LocalTimeOfDay::new(hour, minute).unwrap()
    }

    fn decide(
        settings: &Settings,
        choice: &SpokenAlertChoice,
        home: bool,
        turn: bool,
        now: LocalTimeOfDay,
    ) -> UnpromptedSpeech {
        decide_spoken_alert(Some(settings), choice, home, turn, "A match", now)
    }

    fn refused(why: SpeechRefusal) -> UnpromptedSpeech {
        UnpromptedSpeech::Refused(why)
    }

    #[test]
    fn a_member_hears_nothing_until_they_opt_in() {
        let choice = SpokenAlertChoice::of(&HashMap::new());
        assert!(!choice.enabled);
        assert_eq!(choice.quiet_start, "22:00");
        assert_eq!(choice.quiet_end, "07:00");
        assert_eq!(
            decide(&household(true), &choice, true, false, at(12, 0)),
            refused(SpeechRefusal::NotOptedIn)
        );
        assert_eq!(
            decide(&household(true), &opted_in(), true, false, at(12, 0)),
            UnpromptedSpeech::Spoken
        );
    }

    #[test]
    fn the_household_switch_and_quiet_hours_still_rule() {
        assert_eq!(
            decide(&household(false), &opted_in(), true, false, at(12, 0)),
            refused(SpeechRefusal::NotEnabled)
        );
        let mut quiet_lunch = household(true);
        quiet_lunch.quiet_hours_start = "12:00".into();
        quiet_lunch.quiet_hours_end = "13:00".into();
        assert_eq!(
            decide(&quiet_lunch, &opted_in(), true, false, at(12, 30)),
            refused(SpeechRefusal::QuietHours)
        );
    }

    #[test]
    fn the_members_own_quiet_hours_wrap_midnight_and_are_editable() {
        let choice = opted_in();
        let mut household = household(true);
        household.quiet_hours_start = "03:00".into();
        household.quiet_hours_end = "04:00".into();
        for (h, m) in [(22, 0), (23, 59), (0, 0), (6, 59)] {
            assert_eq!(
                decide(&household, &choice, true, false, at(h, m)),
                refused(SpeechRefusal::MemberQuietHours),
                "{h}:{m}"
            );
        }
        assert_eq!(
            decide(&household, &choice, true, false, at(7, 0)),
            UnpromptedSpeech::Spoken
        );
        let late = SpokenAlertChoice {
            quiet_start: "23:30".into(),
            ..choice.clone()
        };
        assert_eq!(
            decide(&household, &late, true, false, at(22, 30)),
            UnpromptedSpeech::Spoken
        );
        let broken = SpokenAlertChoice {
            quiet_start: "noon".into(),
            ..choice
        };
        assert_eq!(
            decide(&household, &broken, true, false, at(12, 0)),
            refused(SpeechRefusal::MemberQuietHours)
        );
    }

    #[test]
    fn nothing_is_said_mid_conversation_or_to_an_empty_room() {
        assert_eq!(
            decide(&household(true), &opted_in(), true, true, at(12, 0)),
            refused(SpeechRefusal::MidConversation)
        );
        assert_eq!(
            decide(&household(true), &opted_in(), false, false, at(12, 0)),
            refused(SpeechRefusal::MemberNotPresent)
        );
        assert_eq!(
            decide_spoken_alert(None, &opted_in(), true, false, "A match", at(12, 0)),
            refused(SpeechRefusal::SettingsUnreadable)
        );
    }

    #[test]
    fn the_choice_round_trips_through_preferences() {
        let mut prefs = HashMap::from([("language".to_string(), "sw".to_string())]);
        let choice = SpokenAlertChoice {
            enabled: true,
            quiet_start: "21:30".into(),
            quiet_end: "06:00".into(),
        };
        choice.write_into(&mut prefs);
        assert_eq!(SpokenAlertChoice::of(&prefs), choice);
        assert_eq!(prefs["language"], "sw", "other preferences are kept");
        assert!(is_time_of_day("06:00") && !is_time_of_day("6pm"));
    }

    #[test]
    fn the_alert_names_the_member_and_its_first_line() {
        assert_eq!(
            spoken_text(
                Some("Amina"),
                "A match on Used cars",
                "New: 2016 Subaru Forester, 1,400,000\nNew: 2018 Forester"
            ),
            "Amina, a match on Used cars: New: 2016 Subaru Forester, 1,400,000."
        );
        assert_eq!(
            spoken_text(None, "2 matches on Cars", ""),
            "2 matches on Cars."
        );
    }

    struct Fixed(Settings);

    #[async_trait]
    impl SettingsRepository for Fixed {
        async fn get(&self) -> anyhow::Result<Settings> {
            Ok(self.0.clone())
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Speaker(Mutex<Vec<String>>);

    #[async_trait]
    impl Announce for Speaker {
        async fn announce(&self, text: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    fn noon() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn a_match_is_said_to_its_member_only_while_presence_says_they_are_home() {
        let profiles = Arc::new(MockProfileRepository::new());
        let amina = profiles
            .create(CreateProfileRequest {
                display_name: "Amina".into(),
                avatar_emoji: "*".into(),
            })
            .await
            .unwrap();
        let mut prefs = HashMap::new();
        opted_in().write_into(&mut prefs);
        profiles.update_preferences(&amina.id, prefs).await.unwrap();
        let presence = SharedPresence::default();
        let speaker = Arc::new(Speaker::default());
        let alerts = SpokenWatchAlerts::new(
            profiles,
            Arc::new(Fixed(household(true))),
            presence.clone(),
            speaker.clone(),
        )
        .with_turn_check(|| false);

        let say = || alerts.speak(&amina.id, "A match on Used cars", "New: Forester", noon());
        assert_eq!(say().await, refused(SpeechRefusal::MemberNotPresent));

        let arrived = ProfilePresence {
            profile_id: amina.id.clone(),
            transition: PresenceTransition::Arrived,
            source: IdentificationSource::Explicit,
            confidence: None,
            session_id: "s1".into(),
            at: noon(),
        };
        record(&presence, &arrived);
        assert_eq!(say().await, UnpromptedSpeech::Spoken);
        assert_eq!(
            speaker.0.lock().unwrap().as_slice(),
            ["Amina, a match on Used cars: New: Forester."]
        );

        record(
            &presence,
            &ProfilePresence {
                transition: PresenceTransition::Departed,
                at: noon() + chrono::Duration::minutes(1),
                ..arrived
            },
        );
        assert_eq!(say().await, refused(SpeechRefusal::MemberNotPresent));
        assert_eq!(speaker.0.lock().unwrap().len(), 1);
    }
}
