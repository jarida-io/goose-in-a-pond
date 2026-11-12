//! Which of a member's interests the news covers, and what it searches for each.

use crate::shared::domain::news::InterestRow;

/// Conversations a guess must rest on before the news treats it as an interest: one question
/// asked once is curiosity, not an interest.
pub const GUESS_EVIDENCE: u32 = 3;

/// Words a search query keeps at most; more narrows a news search to nothing.
const QUERY_WORDS: usize = 10;

/// The interests the news may cover: confirmed ones and well-evidenced guesses, never one the
/// member wants avoided or switched off, the deepest first.
pub fn covered(rows: &[InterestRow]) -> Vec<InterestRow> {
    let mut kept: Vec<InterestRow> = rows
        .iter()
        .filter(|r| !r.avoid && (r.news.tiles() || r.news.digest()))
        .filter(|r| r.confirmed || r.evidence >= GUESS_EVIDENCE)
        .filter(|r| !r.topic.trim().is_empty())
        .cloned()
        .collect();
    kept.sort_by_key(|r| (depth_rank(&r.depth), !r.confirmed));
    kept
}

fn depth_rank(depth: &str) -> u8 {
    match depth {
        "devoted" => 0,
        "keen" => 1,
        _ => 2,
    }
}

/// The searches for one interest, most specific first: one facet with the topic (a different
/// facet each `turn`, so passes dig into each), then the topic alone.
pub fn queries_for(interest: &InterestRow, turn: usize) -> Vec<String> {
    let topic = clip_words(interest.topic.trim(), QUERY_WORDS);
    let facets: Vec<&str> = interest
        .facets
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .collect();
    let mut queries = Vec::new();
    if !facets.is_empty() {
        let facet = facets[turn % facets.len()];
        let query = if contains_words(&topic, facet) {
            topic.clone()
        } else if contains_words(facet, &topic) {
            facet.to_string()
        } else {
            format!("{facet} {topic}")
        };
        queries.push(clip_words(&query, QUERY_WORDS));
    }
    if !queries.contains(&topic) {
        queries.push(topic);
    }
    queries
}

fn clip_words(text: &str, max: usize) -> String {
    text.split_whitespace()
        .take(max)
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_words(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::news::NewsUse;

    fn interest(topic: &str, news: NewsUse) -> InterestRow {
        InterestRow {
            id: topic.to_lowercase().replace(' ', "-"),
            topic: topic.into(),
            category: "other".into(),
            facets: vec![],
            depth: "keen".into(),
            news,
            avoid: false,
            evidence: 1,
            confirmed: true,
        }
    }

    #[test]
    fn guesses_need_three_conversations_and_avoided_or_off_rows_never_count() {
        let mut guess = interest("P vs NP", NewsUse::Both);
        guess.confirmed = false;
        guess.evidence = 2;
        let mut strong_guess = interest("Mini Cooper engineering", NewsUse::Both);
        strong_guess.confirmed = false;
        strong_guess.evidence = 3;
        let mut avoided = interest("Primetime", NewsUse::Both);
        avoided.avoid = true;
        let off = interest("GTA V", NewsUse::Off);
        let kept = covered(&[
            guess,
            strong_guess,
            avoided,
            off,
            interest("The Boys", NewsUse::Tiles),
        ]);
        let topics: Vec<&str> = kept.iter().map(|r| r.topic.as_str()).collect();
        assert_eq!(topics, vec!["The Boys", "Mini Cooper engineering"]);
    }

    #[test]
    fn the_deepest_interests_come_first() {
        let mut devoted = interest("Amapiano in Nairobi", NewsUse::Both);
        devoted.depth = "devoted".into();
        let mut curious = interest("Willem Dafoe", NewsUse::Both);
        curious.depth = "curious".into();
        let kept = covered(&[curious, interest("MCP", NewsUse::Both), devoted]);
        let topics: Vec<&str> = kept.iter().map(|r| r.topic.as_str()).collect();
        assert_eq!(topics, vec!["Amapiano in Nairobi", "MCP", "Willem Dafoe"]);
    }

    #[test]
    fn queries_turn_through_the_facets_then_fall_back_to_the_topic() {
        let mut boys = interest("The Boys", NewsUse::Both);
        boys.facets = vec!["Gen V".into(), "Homelander".into()];
        assert_eq!(queries_for(&boys, 0), vec!["Gen V The Boys", "The Boys"]);
        assert_eq!(
            queries_for(&boys, 1),
            vec!["Homelander The Boys", "The Boys"]
        );
        assert_eq!(queries_for(&boys, 2)[0], "Gen V The Boys");
        let plain = interest("Model Context Protocol", NewsUse::Both);
        assert_eq!(queries_for(&plain, 5), vec!["Model Context Protocol"]);
    }

    #[test]
    fn a_facet_that_repeats_the_topic_is_not_said_twice() {
        let mut cars = interest("Toyota Fielder prices in Kenya", NewsUse::Both);
        cars.facets = vec!["toyota fielder".into()];
        assert_eq!(
            queries_for(&cars, 0),
            vec!["Toyota Fielder prices in Kenya"]
        );
    }
}
