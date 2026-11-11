//! The news prompts on a real Gemma 4 through a local Ollama, with real pages read the way the
//! pond reads them. Run by hand: `cargo test -p pond-mcp-server --test news_live -- --ignored
//! --nocapture --test-threads=1`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::domain::news::{InterestRow, NewsSettings, NewsUse, Portrait};
use pond_core::shared::mocks::mock_news::MockNewsStore;
use pond_core::shared::ports::news::{
    FoundPage, Freshness, NewsStore, NewsWeb, ReadPage, SearchBlocked, SearchFailure,
};
use pond_core::shared::services::news::schedule::StoryPass;
use pond_core::shared::services::news::topic_map::route;
use pond_core::shared::services::news::writer::{MapExtras, NewsWriter, PassLimits, Today};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_mcp_server::news_web::BraveNewsWeb;

const OLLAMA: &str = "http://127.0.0.1:11434/api/chat";
const MODEL: &str = "gemma4:latest";

struct Ollama {
    client: reqwest::Client,
}

#[async_trait]
impl LlmProvider for Ollama {
    async fn complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        let mut turns = vec![serde_json::json!({"role": "system", "content": system})];
        for m in messages {
            turns.push(serde_json::json!({"role": "user", "content": m.content}));
        }
        let started = std::time::Instant::now();
        let reply: serde_json::Value = self
            .client
            .post(OLLAMA)
            .json(&serde_json::json!({
                "model": MODEL,
                "messages": turns,
                "stream": false,
                "options": {"temperature": 0.7},
            }))
            .send()
            .await?
            .json()
            .await?;
        let content = reply["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        eprintln!(
            "[ollama] {:.1}s, {} chars",
            started.elapsed().as_secs_f32(),
            content.len()
        );
        Ok(ChatMessage::assistant(content))
    }

    fn model_name(&self) -> String {
        MODEL.into()
    }
}

fn ollama() -> Ollama {
    Ollama {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(900))
            .build()
            .unwrap(),
    }
}

fn interest(id: &str, topic: &str, category: &str, facets: &[&str], depth: &str) -> InterestRow {
    InterestRow {
        id: id.into(),
        topic: topic.into(),
        category: category.into(),
        facets: facets.iter().map(|f| f.to_string()).collect(),
        depth: depth.into(),
        news: NewsUse::Both,
        avoid: false,
        evidence: 4,
        confirmed: true,
    }
}

/// Jerry's interests as the interview would leave them, from the nano's real data.
fn jerrys_interests() -> Vec<InterestRow> {
    let mut primetime = interest(
        "primetime",
        "Primetime (thriller)",
        "film_tv",
        &[],
        "curious",
    );
    primetime.confirmed = false;
    primetime.evidence = 1;
    vec![
        interest(
            "mcp",
            "MCP and on-device AI",
            "tech",
            &[
                "Model Context Protocol",
                "Gemma 4",
                "Jetson Orin Nano",
                "on-device inference",
            ],
            "devoted",
        ),
        interest(
            "cars",
            "Kenyan used-car market",
            "cars",
            &["Toyota Fielder", "Autochek", "Jiji", "Kilimall"],
            "keen",
        ),
        interest(
            "boys",
            "The Boys and Gen V",
            "film_tv",
            &["The Boys", "Gen V", "Vought Rising"],
            "keen",
        ),
        interest(
            "politics",
            "Kenyan civic politics",
            "politics",
            &["Linda Mwananchi Movement", "Edwin Sifuna"],
            "keen",
        ),
        interest(
            "amapiano",
            "Amapiano and Nairobi nightlife",
            "music",
            &["Kolacopia", "Amapiano events in Nairobi"],
            "keen",
        ),
        interest(
            "games",
            "Game karma and wanted systems",
            "games",
            &["inFAMOUS", "GTA V", "Rockstar Games"],
            "curious",
        ),
        interest(
            "actors",
            "Actors",
            "film_tv",
            &[
                "Willem Dafoe",
                "Patrick Stewart",
                "Robert Downey Jr.",
                "Jon Hamm",
            ],
            "curious",
        ),
        interest("avengers", "Avengers: Doomsday", "film_tv", &[], "curious"),
        interest("pnp", "P vs NP", "science", &[], "curious"),
        interest(
            "mini",
            "Mini Cooper engineering",
            "cars",
            &["fake turbo whistles"],
            "curious",
        ),
        primetime,
    ]
}

fn jerrys_portrait() -> Portrait {
    Portrait {
        summary: "Jerry Anyumba is CEO of Jarida One Ltd and team lead of Jarida, an open-source, \
                  privacy-first AI company in Nairobi building Goose In A Pond, an offline \
                  smart-home AI. He wants information structured, categorised and scored."
            .into(),
        into: vec![
            "Model Context Protocol".into(),
            "on-device AI".into(),
            "Kenyan civic politics".into(),
            "Amapiano".into(),
        ],
        working_on: vec![
            "Goose In A Pond".into(),
            "his talk at MCP Dev Summit Nairobi, 19-20 November".into(),
            "buying a Toyota Fielder under KSh 1.2M".into(),
        ],
        likes: vec![
            "teal".into(),
            "The Boys".into(),
            "Amapiano nights out".into(),
        ],
    }
}

fn jerrys_memories() -> Vec<String> {
    [
        "Jerry is speaking at MCP Dev Summit Nairobi on 19-20 November.",
        "Jerry is shopping for a Toyota Fielder under KSh 1.2 million on Kilimall, Autochek and Jiji.",
        "Jerry lives with his best friend Emmanuel Oyugi.",
        "Jerry is lactose intolerant.",
        "Jerry dislikes fake-turbo exhaust whistles on cars.",
        "Jerry speaks English and Swahili.",
        "Jerry compares inFAMOUS karma systems with Rockstar's wanted levels in GTA V.",
        "Jerry Anyumba tracks detailed biographies and career milestones of public figures such as Willem Dafoe.",
        "Jerry Anyumba inquires about the new psychological thriller titled Primetime.",
        "Jerry Anyumba is currently working through a specific assignment named Avengers Doomsday.",
        "Jarida One Ltd's quarterly revenue and staff reviews are due in December.",
    ]
    .iter()
    .map(|m| m.to_string())
    .collect()
}

async fn jerry(profiles: &MockProfileRepository) -> String {
    let profile = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry Anyumba".into(),
            avatar_emoji: "goose".into(),
        })
        .await
        .unwrap();
    let mut prefs = HashMap::new();
    prefs.insert("preferred_name".to_string(), "Jerry".to_string());
    profiles
        .update_preferences(&profile.id, prefs)
        .await
        .unwrap();
    profile.id
}

#[tokio::test]
#[ignore = "needs Ollama with gemma4:latest on 127.0.0.1:11434"]
async fn topic_map_on_gemma() {
    let profiles = Arc::new(MockProfileRepository::new());
    let member = jerry(&profiles).await;
    let store = Arc::new(MockNewsStore::new().with_interests(&member, jerrys_interests()));
    store
        .portraits
        .lock()
        .unwrap()
        .insert(member.clone(), jerrys_portrait());
    let writer = NewsWriter::new(store.clone(), Arc::new(BraveNewsWeb::default()), profiles);
    let now = Utc::now();
    let limits = PassLimits {
        budget: Duration::from_secs(1800),
        call_timeout: Duration::from_secs(900),
        ..PassLimits::stories()
    };
    let report = writer
        .build_map(
            &ollama(),
            &member,
            MapExtras {
                memories: jerrys_memories(),
                routines: vec![
                    "Every weekday at 07:30: give me my daily brief: Nairobi weather, my calendar, and anything new on MCP.".into(),
                ],
            },
            now,
            &Today::of(now, chrono_tz::Africa::Nairobi),
            &limits,
        )
        .await
        .unwrap();
    println!("{report:?}");
    let rows = jerrys_interests();
    for topic in store.topics(&member).await.unwrap() {
        let routed = route(&topic, &rows)
            .map(|r| r.interest.topic)
            .unwrap_or_else(|| "(dropped)".into());
        println!(
            "{:>5.1} | {} | {} | {} | {} | {} | -> {}",
            topic.score,
            if topic.newsworthy {
                "news"
            } else {
                "evergreen"
            },
            topic.area,
            topic.subject,
            topic.query,
            topic.why,
            routed
        );
    }
}

/// Finds fixed pages for any query naming its interest's words, and reads them for real.
struct FixedWeb {
    pages: Vec<(&'static str, Vec<&'static str>)>,
    reader: BraveNewsWeb,
}

#[async_trait]
impl NewsWeb for FixedWeb {
    async fn blocked(&self) -> Option<SearchBlocked> {
        None
    }

    async fn find(&self, query: &str, _: Freshness) -> Result<Vec<FoundPage>, SearchFailure> {
        let query = query.to_lowercase();
        Ok(self
            .pages
            .iter()
            .filter(|(word, _)| query.contains(word))
            .flat_map(|(_, urls)| urls.iter())
            .map(|url| FoundPage {
                url: url.to_string(),
                title: String::new(),
            })
            .collect())
    }

    async fn read(&self, url: &str) -> Option<ReadPage> {
        let page = self.reader.read(url).await;
        match &page {
            Some(p) => eprintln!(
                "[read] {} {} chars, published {:?}",
                p.site,
                p.lines.iter().map(|l| l.len()).sum::<usize>(),
                p.published_at
            ),
            None => eprintln!("[read] could not read {url}"),
        }
        page
    }

    async fn picture(&self, _: &str) -> Option<String> {
        None
    }
}

#[tokio::test]
#[ignore = "needs Ollama with gemma4:latest on 127.0.0.1:11434 and the public web"]
async fn stories_and_digest_on_gemma() {
    let profiles = Arc::new(MockProfileRepository::new());
    let member = jerry(&profiles).await;
    let interests: Vec<InterestRow> = jerrys_interests()
        .into_iter()
        .filter(|i| ["cars", "mcp", "politics"].contains(&i.id.as_str()))
        .collect();
    let store = Arc::new(MockNewsStore::new().with_interests(&member, interests));
    store
        .portraits
        .lock()
        .unwrap()
        .insert(member.clone(), jerrys_portrait());
    let web = Arc::new(FixedWeb {
        pages: vec![
            ("civic", vec![
                "https://nation.africa/kenya/news/politics/sifuna-wants-case-seeking-details-of-linda-mwananchi-funds-dismissed--5664486",
                "https://mulembe.co.ke/2026/11/09/mudavadi-writes-off-uhuru-experiment-on-sifuna-in-his-western-turf/",
                "https://www.the-star.co.ke/news/2026-11-05-talarico-stole-linda-mwananchi-2027-victory-speech-sifuna",
            ]),
            ("used-car", vec![
                "https://garisea.com/research/car-prices/toyota/fielder",
                "https://businessradar.co.ke/?p=14387",
            ]),
            ("mcp", vec![
                "https://news.lavx.hu/article/aaif-s-mcp-dev-summit-gateways-grpc-and-observability-signal-protocol-hardening",
                "https://www.developersdigest.tech/blog/mcp-2026-07-28-breaking-changes",
            ]),
            ("boys", vec![
                "https://collider.com/the-boys-spin-off-vought-rising-2027-release-window/",
                "https://tbreak.com/gen-v-canceled-vought-rising-2027/",
            ]),
            ("amapiano", vec![
                "https://www.standardmedia.co.ke/business/entertainment/article/2001528113/musa-keys-south-african-amapiano-star-to-headline-road-to-africa-experience-in-nairobi",
            ]),
        ],
        reader: BraveNewsWeb::default(),
    });
    let writer = NewsWriter::new(store.clone(), web, profiles);
    let now = Utc::now();
    let today = Today::of(now, chrono_tz::Africa::Nairobi);
    let limits = PassLimits {
        budget: Duration::from_secs(3600),
        call_timeout: Duration::from_secs(900),
        per_interest: 1,
        per_pass: 3,
        topics_per_pass: 6,
    };
    let model = ollama();
    let report = writer
        .write_stories(
            &model,
            &member,
            StoryPass::Refresh,
            &NewsSettings::default(),
            now,
            &today,
            &limits,
        )
        .await
        .unwrap();
    println!("{report:?}");
    for story in store.stories_of(&member) {
        println!(
            "\n=== [{}] {}\n{}\nwhy: {}\nsource: {} {}\n\n{}\n",
            story.interest.topic,
            story.headline,
            story.dek,
            story.why_you,
            story.site,
            story.url,
            story.summary
        );
    }
    let digest = writer
        .write_digest(
            &model,
            &member,
            &NewsSettings::default(),
            now,
            &today,
            &limits,
        )
        .await
        .unwrap()
        .expect("a digest");
    println!("\n##### {}\n{}\n", digest.title, digest.intro);
    for section in &digest.sections {
        println!(
            "## {} [{}]\n{}\nrefs: {:?}\n",
            section.heading,
            section.interest.topic,
            section.body,
            section
                .references
                .iter()
                .map(|r| &r.url)
                .collect::<Vec<_>>()
        );
    }
}
