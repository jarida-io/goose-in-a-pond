use super::*;
use crate::db::Database;
use chrono::TimeZone;
use pond_core::shared::domain::news::{DigestPicture, DigestSection};

async fn store() -> (SqliteNewsStore, Pool<Sqlite>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    for (id, name) in [("jerry", "Jerry"), ("emmanuel", "Emmanuel")] {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(&db.system)
            .await
            .unwrap();
    }
    (
        SqliteNewsStore::new(db.system.clone()),
        db.system.clone(),
        tmp,
    )
}

/// One `interests` row as the interests code writes it, with SQLite's own timestamps.
#[allow(clippy::too_many_arguments)]
async fn seed_interest(
    pool: &Pool<Sqlite>,
    id: &str,
    member: &str,
    topic: &str,
    facets: &str,
    news: &str,
    avoid: bool,
    evidence: i64,
    confirmed: bool,
) {
    sqlx::query(
        "INSERT INTO interests (id, profile_id, topic, topic_key, category, facets, depth, news, \
         avoid, origin, evidence, confirmed, created_at, updated_at) \
         VALUES (?, ?, ?, lower(?), 'tech', ?, 'keen', ?, ?, 'chat', ?, ?, datetime('now'), \
         datetime('now'))",
    )
    .bind(id)
    .bind(member)
    .bind(topic)
    .bind(topic)
    .bind(facets)
    .bind(news)
    .bind(avoid as i64)
    .bind(evidence)
    .bind(confirmed as i64)
    .execute(pool)
    .await
    .unwrap();
}

fn at(h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 9, h, 0, 0).unwrap()
}

fn story(id: &str, member: &str, url: &str, interest: &str, written: DateTime<Utc>) -> Story {
    Story {
        id: id.into(),
        profile_id: member.into(),
        kind: StoryKind::News,
        interest: InterestRef {
            id: interest.into(),
            topic: interest.into(),
        },
        headline: format!("Headline {id}"),
        dek: "Dek.".into(),
        summary: "Summary.".into(),
        why_you: "Why.".into(),
        url: url.into(),
        site: "Nation".into(),
        source_title: "Page title".into(),
        further_sources: vec![Reference {
            title: "Second page".into(),
            url: "https://b.example/2".into(),
            site: "b.example".into(),
        }],
        picture_key: Some("abcdef0123456789".into()),
        found_by: Some("Linda Mwananchi Movement".into()),
        published_at: Some(at(1)),
        written_at: written,
        read_at: None,
        vote: None,
    }
}

#[tokio::test]
async fn settings_default_until_saved_and_the_book_follows_a_pass() {
    let (news, _pool, _tmp) = store().await;
    assert_eq!(
        news.settings("jerry").await.unwrap(),
        NewsSettings::default()
    );
    let mine = NewsSettings {
        digest_time: "06:30".into(),
        spoken_on_speaker: true,
        timezone: Some("Africa/Nairobi".into()),
        ..Default::default()
    };
    news.save_settings("jerry", &mine).await.unwrap();
    assert_eq!(news.settings("jerry").await.unwrap(), mine);
    assert_eq!(
        news.settings("emmanuel").await.unwrap(),
        NewsSettings::default()
    );

    news.request_refresh("jerry", at(9)).await.unwrap();
    assert_eq!(
        news.book("jerry").await.unwrap().refresh_requested_at,
        Some(at(9))
    );
    news.begin_writing("jerry", at(10)).await.unwrap();
    let book = news.book("jerry").await.unwrap();
    assert_eq!(
        (book.refresh_requested_at, book.writing_since),
        (None, Some(at(10)))
    );
    news.end_writing("jerry", Some(at(11))).await.unwrap();
    news.mark_spoken("jerry", at(11).date_naive())
        .await
        .unwrap();
    let book = news.book("jerry").await.unwrap();
    assert_eq!(book.writing_since, None);
    assert_eq!(book.stories_written_at, Some(at(11)));
    assert_eq!(book.spoken_on, Some(at(11).date_naive()));
    assert_eq!(
        news.settings("jerry").await.unwrap(),
        mine,
        "bookkeeping leaves the choices"
    );
}

#[tokio::test]
async fn interests_and_the_portrait_read_as_the_other_tables_write_them() {
    let (news, pool, _tmp) = store().await;
    seed_interest(
        &pool,
        "i1",
        "jerry",
        "MCP",
        r#"["Gemma 4","Jetson"]"#,
        "both",
        false,
        1,
        true,
    )
    .await;
    seed_interest(
        &pool,
        "i2",
        "jerry",
        "Primetime",
        "[]",
        "tiles",
        true,
        5,
        false,
    )
    .await;
    seed_interest(
        &pool, "i3", "emmanuel", "Football", "[]", "both", false, 1, true,
    )
    .await;
    sqlx::query(
        "INSERT INTO member_portraits (profile_id, summary, into_json, working_on_json, \
         likes_json, updated_at) VALUES ('jerry', 'Runs Jarida.', '[\"MCP\"]', '[\"GIAP\"]', '[]', \
         datetime('now'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    let rows = news.interests("jerry").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].topic, "MCP");
    assert_eq!(rows[0].facets, vec!["Gemma 4", "Jetson"]);
    assert_eq!(rows[0].news, NewsUse::Both);
    assert!(rows[1].avoid && !rows[1].confirmed && rows[1].evidence == 5);
    let portrait = news.portrait("jerry").await.unwrap().unwrap();
    assert_eq!(portrait.summary, "Runs Jarida.");
    assert_eq!(portrait.working_on, vec!["GIAP"]);
    assert_eq!(news.portrait("emmanuel").await.unwrap(), None);
    assert!(news
        .book("jerry")
        .await
        .unwrap()
        .interests_changed_at
        .is_some());
    assert_eq!(news.members().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_page_is_kept_once_and_lists_leave_out_what_was_called_wrong() {
    let (news, _pool, _tmp) = store().await;
    let first = story(
        "s1",
        "jerry",
        "https://www.nation.africa/a?utm_source=x",
        "politics",
        at(6),
    );
    assert!(news.add_story(&first).await.unwrap());
    assert!(!news
        .add_story(&story(
            "s9",
            "jerry",
            "https://nation.africa/a",
            "politics",
            at(7)
        ))
        .await
        .unwrap());
    assert!(news
        .add_story(&story(
            "s1e",
            "emmanuel",
            "https://nation.africa/a",
            "x",
            at(7)
        ))
        .await
        .unwrap());
    news.add_story(&story("s2", "jerry", "https://b.example/2", "cars", at(8)))
        .await
        .unwrap();
    news.add_story(&story(
        "s3",
        "jerry",
        "https://c.example/3",
        "politics",
        at(9),
    ))
    .await
    .unwrap();

    let since: Vec<String> = news
        .stories_since("jerry", at(7))
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(since, vec!["s3", "s2"]);
    assert_eq!(news.story("jerry", "s1").await.unwrap().unwrap(), first);
    assert!(news.story("emmanuel", "s1").await.unwrap().is_none());
    assert!(news
        .has_page("jerry", &url_key("http://nation.africa/a/"))
        .await
        .unwrap());

    assert!(news
        .set_vote("jerry", "s3", Vote::Wrong, Some("not about Sifuna"), at(10))
        .await
        .unwrap());
    assert!(news
        .set_vote("jerry", "s2", Vote::Up, None, at(10))
        .await
        .unwrap());
    assert!(!news
        .set_vote("jerry", "s1e", Vote::Up, None, at(10))
        .await
        .unwrap());
    let politics: Vec<String> = news
        .interest_stories("jerry", "politics", 30)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(politics, vec!["s1"]);
    let votes = news.voted_stories("jerry", at(0)).await.unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].vote, Some(Vote::Up));

    assert!(news.mark_read("jerry", "s2", at(11)).await.unwrap());
    assert!(!news.mark_read("jerry", "nope", at(11)).await.unwrap());
    let read = news.read_stories("jerry", at(0)).await.unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].read_at, Some(at(11)));

    assert_eq!(
        news.count_stories("jerry", StoryKind::News, at(0))
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        news.count_stories("jerry", StoryKind::LongRead, at(0))
            .await
            .unwrap(),
        0
    );
    assert_eq!(news.prune_stories(at(7)).await.unwrap(), 1);
}

#[tokio::test]
async fn every_digest_is_kept_one_a_day() {
    let (news, _pool, _tmp) = store().await;
    let digest = |id: &str, day: u32, h: u32| Digest {
        id: id.into(),
        profile_id: "jerry".into(),
        day: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
        title: format!("Title {id}"),
        intro: "One. Two.".into(),
        sections: vec![DigestSection {
            interest: InterestRef {
                id: "politics".into(),
                topic: "Kenyan civic politics".into(),
            },
            heading: "Heading".into(),
            body: "Body [1](https://a.example).".into(),
            pictures: vec![DigestPicture {
                key: "abcdef0123456789".into(),
                caption: "Caption".into(),
                source_url: "https://a.example".into(),
            }],
            references: vec![],
        }],
        written_at: Utc.with_ymd_and_hms(2026, 11, day, h, 0, 0).unwrap(),
    };
    news.add_digest(&digest("d8", 8, 4)).await.unwrap();
    news.add_digest(&digest("d9", 9, 4)).await.unwrap();
    news.add_digest(&digest("d9b", 9, 5)).await.unwrap();
    let list: Vec<String> = news
        .digests("jerry")
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(list, vec!["d9", "d8"]);
    assert_eq!(
        news.digest("jerry", "d8").await.unwrap().unwrap(),
        digest("d8", 8, 4)
    );
    assert!(news.digest("emmanuel", "d8").await.unwrap().is_none());
    assert_eq!(news.latest_digest("jerry").await.unwrap().unwrap().id, "d9");
    assert_eq!(
        news.digest_on("jerry", NaiveDate::from_ymd_opt(2026, 11, 8).unwrap())
            .await
            .unwrap()
            .unwrap()
            .id,
        "d8"
    );
}

#[tokio::test]
async fn a_rebuilt_map_keeps_what_the_ideas_used() {
    let (news, _pool, _tmp) = store().await;
    let topic = |id: &str, query: &str, score: f64, built: DateTime<Utc>| Topic {
        id: id.into(),
        profile_id: "jerry".into(),
        area: "MCP and on-device AI".into(),
        subject: "MCP".into(),
        query: query.into(),
        newsworthy: false,
        score,
        why: "interests".into(),
        built_at: built,
        used_at: None,
    };
    news.replace_topics(
        "jerry",
        &[
            topic("t1", "MCP Tasks spec", 80.0, at(1)),
            topic("t2", "Gemma 4 E4B", 60.0, at(1)),
        ],
    )
    .await
    .unwrap();
    news.mark_topics_used("jerry", &["t1".into()], at(2))
        .await
        .unwrap();
    news.replace_topics(
        "jerry",
        &[
            topic("n1", "mcp tasks spec", 90.0, at(3)),
            topic("n2", "Jetson Orin Nano", 50.0, at(3)),
        ],
    )
    .await
    .unwrap();
    let map = news.topics("jerry").await.unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!((map[0].id.as_str(), map[0].used_at), ("t1", Some(at(2))));
    assert_eq!(map[0].score, 90.0);
    assert_eq!((map[1].id.as_str(), map[1].used_at), ("n2", None));
    assert_eq!(news.book("jerry").await.unwrap().map_built_at, Some(at(3)));
}
