use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::TimeZone;

use super::*;
use crate::models::ports::provider::LlmProvider;
use crate::shared::domain::news::NewsUse;
use crate::shared::mocks::mock_news::{MockNewsStore, MockNewsWeb};
use crate::user_data::domain::profile::CreateProfileRequest;
use crate::user_data::mocks::mock_profile::MockProfileRepository;

const MEMBER_TEXT: &str = "The page reports what happened this week in plenty of detail, \
    with names and places and what comes next for everyone involved in it.";

/// Writes a story from whatever page title the prompt carries, a section citing every source,
/// and an intro; counts its calls.
#[derive(Default)]
struct Scripted {
    calls: AtomicUsize,
    prompts: std::sync::Mutex<Vec<String>>,
}

fn words(n: usize) -> String {
    vec!["detail"; n].join(" ")
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let prompt = messages[0].content.clone();
        self.prompts.lock().unwrap().push(prompt.clone());
        let reply = if system == STORY_SYSTEM {
            let title = prompt
                .lines()
                .find_map(|l| l.strip_prefix("Title: "))
                .unwrap_or("Nothing");
            serde_json::json!({
                "headline": format!("{title} reported in full"),
                "dek": format!("What {title} means."),
                "summary": words(130),
                "why_you": "You follow this closely."
            })
            .to_string()
        } else if system == LONG_READ_SYSTEM {
            serde_json::json!({
                "headline": "How P vs NP is still open",
                "dek": "What the question asks and why no proof has held.",
                "summary": format!("{} [1].\n\n{} [2].", words(160), words(160)),
                "why_you": "You keep coming back to P vs NP."
            })
            .to_string()
        } else if system == SECTION_SYSTEM {
            let topic = prompt
                .lines()
                .find_map(|l| l.strip_prefix("The interest: "))
                .unwrap_or("it");
            serde_json::json!({
                "heading": format!("News on {}", topic.split(" (").next().unwrap().trim_end_matches(['.', ' '])).chars().take(60).collect::<String>(),
                "body": format!("{} [1].", words(60)),
            })
            .to_string()
        } else {
            r#"{"title": "Two big things", "intro": "You have news. It is good."}"#.to_string()
        };
        Ok(ChatMessage::assistant(reply))
    }

    fn model_name(&self) -> String {
        "scripted".into()
    }
}

fn interest(id: &str, topic: &str, news: NewsUse) -> InterestRow {
    InterestRow {
        id: id.into(),
        topic: topic.into(),
        category: "other".into(),
        facets: vec![],
        depth: "keen".into(),
        news,
        avoid: false,
        evidence: 3,
        confirmed: true,
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 9, 3, 10, 0).unwrap()
}

fn today() -> Today {
    Today::of(now(), chrono_tz::Africa::Nairobi)
}

fn text(n: usize) -> String {
    vec![MEMBER_TEXT; n].join("\n")
}

async fn writer(store: Arc<MockNewsStore>, web: Arc<MockNewsWeb>) -> (NewsWriter, String) {
    let profiles = Arc::new(MockProfileRepository::new());
    let jerry = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry Anyumba".into(),
            avatar_emoji: "goose".into(),
        })
        .await
        .unwrap();
    let mut prefs = HashMap::new();
    prefs.insert("preferred_name".to_string(), "Jerry".to_string());
    profiles.update_preferences(&jerry.id, prefs).await.unwrap();
    store.members.lock().unwrap().clear();
    let interests = store
        .interests
        .lock()
        .unwrap()
        .remove("jerry")
        .unwrap_or_default();
    store
        .interests
        .lock()
        .unwrap()
        .insert(jerry.id.clone(), interests);
    (NewsWriter::new(store, web, profiles), jerry.id)
}

#[tokio::test]
async fn a_member_with_no_interest_gets_no_search_and_no_story() {
    let mut guess = interest("g", "Primetime", NewsUse::Both);
    guess.confirmed = false;
    guess.evidence = 1;
    let store = Arc::new(MockNewsStore::new().with_interests("jerry", vec![guess]));
    let web = Arc::new(MockNewsWeb::new());
    let (news, member) = writer(store.clone(), web.clone()).await;
    let model = Scripted::default();
    let report = news
        .write_stories(
            &model,
            &member,
            StoryPass::Morning,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!(report.groups, 0);
    assert!(web.searches.lock().unwrap().is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn no_search_provider_spends_nothing() {
    let store = Arc::new(
        MockNewsStore::new().with_interests("jerry", vec![interest("mcp", "MCP", NewsUse::Both)]),
    );
    let web = Arc::new(MockNewsWeb {
        blocked: Some(SearchBlocked::NotConfigured),
        ..Default::default()
    });
    let (news, member) = writer(store, web.clone()).await;
    let report = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Refresh,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!(report.blocked, Some(SearchBlocked::NotConfigured));
    assert!(web.searches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stories_take_turns_between_interests_within_the_limits() {
    let store = Arc::new(MockNewsStore::new().with_interests(
        "jerry",
        vec![
            interest("mcp", "MCP", NewsUse::Both),
            interest("boys", "The Boys", NewsUse::Tiles),
        ],
    ));
    let mut web = MockNewsWeb::new();
    let mcp = [
        "Registry opens",
        "Spec drops sessions",
        "Gemma speaks tools",
        "Summit agenda out",
        "SDK grows",
    ];
    let boys = [
        "Homelander returns",
        "Vought Rising casts",
        "Finale ratings soar",
        "Kripke talks ending",
        "Gen V cancelled",
    ];
    for i in 0..5 {
        web = web.with_page("MCP", &format!("https://mcp.example/{i}"), mcp[i], &text(8));
        web = web.with_page(
            "The Boys",
            &format!("https://tv.example/{i}"),
            boys[i],
            &text(8),
        );
    }
    let web = Arc::new(web.with_picture("https://mcp.example/0", "abcdef0123456789"));
    let (news, member) = writer(store.clone(), web.clone()).await;
    let limits = PassLimits {
        per_pass: 5,
        ..PassLimits::stories()
    };
    let report = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Midday,
            &NewsSettings::default(),
            now(),
            &today(),
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(report.written, 5);
    let stories = store.stories_of(&member);
    let topics: Vec<&str> = stories.iter().map(|s| s.interest.topic.as_str()).collect();
    assert_eq!(topics, vec!["MCP", "The Boys", "MCP", "The Boys", "MCP"]);
    let first = &stories[0];
    assert_eq!(first.headline, "Registry opens reported in full");
    assert_eq!(first.source_title, "Registry opens");
    assert_eq!(first.picture_key.as_deref(), Some("abcdef0123456789"));
    assert_eq!(first.site, "mcp.example");
    // Each interest is searched over the day, then the week when the day gave too little.
    let searches = web.searches.lock().unwrap().clone();
    assert_eq!(searches[0], ("MCP".to_string(), Freshness::Day));
}

#[tokio::test]
async fn a_page_already_written_or_the_same_news_again_is_not_written_twice() {
    let store = Arc::new(
        MockNewsStore::new().with_interests("jerry", vec![interest("mcp", "MCP", NewsUse::Both)]),
    );
    let web = Arc::new(
        MockNewsWeb::new()
            .with_page("MCP", "https://mcp.example/a", "Spec adds tasks", &text(8))
            .with_page(
                "MCP",
                "https://other.example/b?utm_source=x",
                "Spec adds tasks",
                &text(8),
            )
            .with_page("MCP", "https://third.example/c", "Registry opens", &text(8)),
    );
    let (news, member) = writer(store.clone(), web).await;
    let model = Scripted::default();
    news.write_stories(
        &model,
        &member,
        StoryPass::Refresh,
        &NewsSettings::default(),
        now(),
        &today(),
        &PassLimits::stories(),
    )
    .await
    .unwrap();
    let headlines: Vec<String> = store
        .stories_of(&member)
        .iter()
        .map(|s| s.headline.clone())
        .collect();
    assert_eq!(
        headlines,
        vec![
            "Spec adds tasks reported in full",
            "Registry opens reported in full"
        ]
    );
    let again = news
        .write_stories(
            &model,
            &member,
            StoryPass::Evening,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!(again.written, 0);
    assert_eq!(again.candidates, 0);
}

#[tokio::test]
async fn a_thin_page_or_a_pass_out_of_time_writes_nothing() {
    let store = Arc::new(
        MockNewsStore::new().with_interests("jerry", vec![interest("mcp", "MCP", NewsUse::Both)]),
    );
    let web = Arc::new(
        MockNewsWeb::new()
            .with_page("MCP", "https://mcp.example/thin", "Thin", "Too short.")
            .with_page("MCP", "https://mcp.example/full", "Full", &text(8)),
    );
    let (news, member) = writer(store.clone(), web).await;
    let out_of_time = PassLimits {
        budget: Duration::ZERO,
        ..PassLimits::stories()
    };
    let report = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Refresh,
            &NewsSettings::default(),
            now(),
            &today(),
            &out_of_time,
        )
        .await
        .unwrap();
    assert!(report.out_of_time);
    assert_eq!(report.written, 0);
    let report = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Refresh,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!((report.unread, report.written), (1, 1));
}

#[tokio::test]
async fn digest_only_interests_are_left_to_the_morning_pass() {
    let store = Arc::new(MockNewsStore::new().with_interests(
        "jerry",
        vec![interest("pol", "Kenyan civic politics", NewsUse::Digest)],
    ));
    let web = Arc::new(MockNewsWeb::new());
    let (news, member) = writer(store, web.clone()).await;
    let midday = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Midday,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!(midday.groups, 0);
    let morning = news
        .write_stories(
            &Scripted::default(),
            &member,
            StoryPass::Morning,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert_eq!(morning.groups, 1);
}

#[tokio::test]
async fn the_digest_has_a_section_per_interest_with_its_pictures_and_references() {
    let store = Arc::new(MockNewsStore::new().with_interests(
        "jerry",
        vec![
            interest("mcp", "MCP", NewsUse::Both),
            interest("cars", "Toyota Fielder prices", NewsUse::Digest),
            interest("boys", "The Boys", NewsUse::Tiles),
        ],
    ));
    let web = Arc::new(
        MockNewsWeb::new()
            .with_page("MCP", "https://mcp.example/a", "Spec adds tasks", &text(8))
            .with_picture("https://mcp.example/a", "abcdef0123456789")
            .with_page(
                "Toyota Fielder prices",
                "https://cars.example/f",
                "Fielder prices",
                &text(8),
            )
            .with_picture("https://cars.example/f", "fedcba9876543210"),
    );
    let (news, member) = writer(store.clone(), web.clone()).await;
    let model = Scripted::default();
    news.write_stories(
        &model,
        &member,
        StoryPass::Midday,
        &NewsSettings::default(),
        now(),
        &today(),
        &PassLimits::stories(),
    )
    .await
    .unwrap();
    assert_eq!(
        store.stories_of(&member).len(),
        1,
        "midday covers tiles interests only"
    );

    let digest = news
        .write_digest(
            &model,
            &member,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::digest(),
        )
        .await
        .unwrap()
        .expect("a digest");
    assert_eq!(digest.title, "Two big things");
    assert_eq!(digest.intro, "You have news. It is good.");
    assert_eq!(digest.day, today().date);
    let topics: Vec<&str> = digest
        .sections
        .iter()
        .map(|s| s.interest.topic.as_str())
        .collect();
    assert_eq!(
        topics,
        vec!["MCP", "Toyota Fielder prices"],
        "The Boys feeds tiles only"
    );
    let mcp = &digest.sections[0];
    assert!(mcp.body.ends_with("[1](https://mcp.example/a)."));
    assert_eq!(mcp.pictures[0].key, "abcdef0123456789");
    assert_eq!(mcp.pictures[0].caption, "Spec adds tasks reported in full");
    assert_eq!(mcp.references[0].title, "Spec adds tasks");
    let cars = &digest.sections[1];
    assert_eq!(
        cars.references[0].url, "https://cars.example/f",
        "extra reading for a section with no story"
    );
    assert_eq!(cars.pictures[0].key, "fedcba9876543210");
    assert_eq!(store.digests.lock().unwrap().len(), 1);
    let story_prompt = model.prompts.lock().unwrap()[0].clone();
    assert!(story_prompt.starts_with("You are writing for Jerry."));
    assert!(story_prompt.contains("curiosity, not an interest"));
}

#[tokio::test]
async fn an_evergreen_topic_becomes_a_long_read_now_and_then() {
    let store = Arc::new(
        MockNewsStore::new()
            .with_interests("jerry", vec![interest("pnp", "P vs NP", NewsUse::Both)]),
    );
    let web = Arc::new(
        MockNewsWeb::new()
            .with_page(
                "P vs NP explained",
                "https://a.example/pnp",
                "P vs NP explained",
                &text(8),
            )
            .with_page(
                "P vs NP explained",
                "https://b.example/pnp",
                "The Clay problem",
                &text(8),
            )
            .with_page(
                "P vs NP explained",
                "https://c.example/pnp",
                "Why proofs fail",
                &text(8),
            ),
    );
    let (news, member) = writer(store.clone(), web.clone()).await;
    store.topics.lock().unwrap().push(Topic {
        id: "t1".into(),
        profile_id: member.clone(),
        area: "Theory of computation".into(),
        subject: "P vs NP".into(),
        query: "P vs NP explained".into(),
        newsworthy: false,
        score: 80.0,
        why: "interests".into(),
        built_at: now(),
        used_at: None,
    });
    let model = Scripted::default();
    let report = news
        .write_stories(
            &model,
            &member,
            StoryPass::Evening,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert!(report.long_read);
    let stories = store.stories_of(&member);
    assert_eq!(stories.len(), 1);
    let long = &stories[0];
    assert_eq!(long.kind, StoryKind::LongRead);
    assert_eq!(long.url, "https://a.example/pnp");
    assert_eq!(long.further_sources.len(), 2);
    assert!(long.summary.contains("[2](https://b.example/pnp)"));
    assert_eq!(long.found_by.as_deref(), Some("P vs NP explained"));
    assert_eq!(web.searches.lock().unwrap()[0].1, Freshness::Any);
    assert!(
        store.topics.lock().unwrap()[0].used_at.is_some(),
        "the ideas leave it for a week"
    );

    let again = news
        .write_stories(
            &model,
            &member,
            StoryPass::Evening,
            &NewsSettings::default(),
            now(),
            &today(),
            &PassLimits::stories(),
        )
        .await
        .unwrap();
    assert!(!again.long_read, "two long reads are days apart");
}

#[test]
fn a_guess_seen_once_is_curiosity_unless_a_followed_interest_names_it() {
    let mut primetime = interest("p", "Primetime (thriller)", NewsUse::Both);
    primetime.confirmed = false;
    primetime.evidence = 1;
    let mut dafoe = interest("d", "Willem Dafoe", NewsUse::Both);
    dafoe.confirmed = false;
    dafoe.evidence = 2;
    let mut actors = interest("a", "Actors", NewsUse::Both);
    actors.facets = vec!["Willem Dafoe".into()];
    let rows = vec![primetime, dafoe, actors.clone()];
    assert_eq!(curiosities(&rows, &[actors]), vec!["Primetime".to_string()]);
}
