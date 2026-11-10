//! Telling one story from another: the same page under a different address, and the same news
//! under a different headline.

use std::collections::HashSet;

/// Query parameters that only say where a click came from.
const TRACKING: &[&str] = &[
    "fbclid", "gclid", "ref", "ref_src", "cmpid", "mc_cid", "mc_eid", "ocid", "igshid",
];

/// Words a headline shares with any other, left out when comparing two.
const COMMON: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "is", "it", "its", "of", "on", "or",
    "the", "to", "with", "new", "after", "over", "how", "why", "what", "says",
];

/// Shared words, as a share of all words, at which two headlines are the same news.
const SAME_NEWS: f64 = 0.6;

/// A page's address with what does not change the page taken off: the scheme, `www.`, a mobile
/// or AMP form, tracking parameters, the fragment and a trailing slash.
pub fn url_key(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return url.trim().to_lowercase();
    };
    let host = parsed.host_str().unwrap_or_default().to_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("amp."))
        .unwrap_or(&host)
        .to_string();
    let path = parsed.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/amp")
        .or_else(|| path.strip_suffix(".amp"))
        .unwrap_or(path);
    let mut params: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_lowercase();
            !k.starts_with("utm_") && !TRACKING.contains(&k.as_str())
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        format!("{host}{path}")
    } else {
        format!("{host}{path}?{query}")
    }
}

/// A headline's distinctive words, lowercased.
pub fn headline_words(headline: &str) -> HashSet<String> {
    headline
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty() && !COMMON.contains(&w.as_str()))
        .collect()
}

/// A headline as one comparable string, for storage beside the story.
pub fn headline_key(headline: &str) -> String {
    let mut words: Vec<String> = headline_words(headline).into_iter().collect();
    words.sort();
    words.join(" ")
}

/// Whether two headlines report the same news: most of their words shared, or one's words all
/// inside the other's.
pub fn same_news(a: &str, b: &str) -> bool {
    let (a, b) = (headline_words(a), headline_words(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = a.intersection(&b).count();
    let smaller = a.len().min(b.len());
    if smaller >= 3 && shared == smaller {
        return true;
    }
    let union = a.union(&b).count();
    shared as f64 / union as f64 >= SAME_NEWS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_page_under_another_address_has_one_key() {
        let key =
            url_key("https://www.nation.africa/kenya/news/politics/sifuna-linda-mwananchi-123/");
        assert_eq!(
            key,
            url_key("http://nation.africa/kenya/news/politics/sifuna-linda-mwananchi-123?utm_source=x&fbclid=y#top")
        );
        assert_eq!(
            url_key("https://m.example.com/story/amp"),
            url_key("https://example.com/story")
        );
        assert_ne!(
            url_key("https://example.com/story?id=1"),
            url_key("https://example.com/story?id=2")
        );
    }

    #[test]
    fn reworded_headlines_of_one_story_are_the_same_news() {
        assert!(same_news(
            "Sifuna launches Linda Mwananchi Movement in Nairobi",
            "Edwin Sifuna launches the Linda Mwananchi Movement in Nairobi"
        ));
        assert!(same_news(
            "Gen V season 2 finale sets up The Boys season 5",
            "The Boys season 5 set up by Gen V season 2 finale"
        ));
        assert!(!same_news(
            "Gen V season 2 finale sets up The Boys season 5",
            "Toyota Fielder prices fall below KSh 1.2 million on Jiji"
        ));
        assert!(!same_news("", "Anything at all"));
    }

    #[test]
    fn a_headline_key_ignores_order_case_and_filler() {
        assert_eq!(
            headline_key("The MCP spec adds Tasks"),
            headline_key("Tasks: MCP spec adds")
        );
    }
}
