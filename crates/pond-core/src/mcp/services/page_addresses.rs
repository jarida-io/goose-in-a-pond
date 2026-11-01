//! The addresses `read_web_page` may fetch: one the person wrote, one a long task's goal names, or
//! a search result. A model may pass the tool any string, and an address it wrote could carry the
//! household's data to whoever owns the host, so only an address one of these gave, or a host
//! one of them names, is read. Held in memory only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a noted address stays readable: longer than a long task usually runs.
pub const ADDRESS_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

/// Addresses held at most; the oldest go first.
pub const MAX_ADDRESSES: usize = 256;

/// Addresses noted from one text at most.
const MAX_ADDRESSES_PER_TEXT: usize = 16;

/// Past this, an address is more likely data than a page.
const MAX_ADDRESS_BYTES: usize = 2_048;

/// A title shorter than this names too little to pick a page by part of it.
const MIN_TITLE_PART_CHARS: usize = 12;

#[derive(Debug, Clone)]
struct Noted {
    address: String,
    key: String,
    host: String,
    /// A search source's title, lowercased; empty for an address the person wrote.
    title: String,
    at: Instant,
}

/// The addresses a page read may fetch, newest last.
#[derive(Debug, Default)]
pub struct PageAddresses {
    noted: Mutex<Vec<Noted>>,
}

impl PageAddresses {
    pub const fn new() -> Self {
        Self {
            noted: Mutex::new(Vec::new()),
        }
    }

    /// Notes every address in the person's own words; returns how many.
    pub fn note_words(&self, text: &str) -> usize {
        self.note_words_at(text, Instant::now())
    }

    /// Notes a search result's address under its title.
    pub fn note_source(&self, address: &str, title: &str) {
        self.note_at(address, title, Instant::now());
    }

    /// The address `asked` names, if a noted source gave it: the address itself, or the newest
    /// noted page on a host `asked` names alone, or on a search source whose title it gives.
    pub fn resolve(&self, asked: &str) -> Option<String> {
        self.resolve_at(asked, Instant::now())
    }

    fn note_words_at(&self, text: &str, now: Instant) -> usize {
        let found = addresses_in(text);
        for address in &found {
            self.note_at(address, "", now);
        }
        found.len()
    }

    fn note_at(&self, address: &str, title: &str, now: Instant) {
        let Some(address) = normalize(address) else {
            return;
        };
        let key = key_of(&address);
        let host = host_of(&address);
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut noted = self.noted.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut noted, now);
        let title = match noted.iter().position(|n| n.key == key) {
            Some(i) => {
                let old = noted.remove(i);
                if title.is_empty() {
                    old.title
                } else {
                    title.to_lowercase()
                }
            }
            None => title.to_lowercase(),
        };
        noted.push(Noted {
            address,
            key,
            host,
            title,
            at: now,
        });
        let excess = noted.len().saturating_sub(MAX_ADDRESSES);
        noted.drain(..excess);
    }

    fn resolve_at(&self, asked: &str, now: Instant) -> Option<String> {
        let asked = asked
            .trim()
            .trim_matches(|c: char| "<>\"'`()[]".contains(c))
            .trim();
        if asked.is_empty() {
            return None;
        }
        let mut noted = self.noted.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut noted, now);
        if let Some(address) = normalize(asked) {
            let key = key_of(&address);
            if let Some(n) = noted.iter().rev().find(|n| n.key == key) {
                return Some(n.address.clone());
            }
            // A path or query nobody gave could be carrying data, so only a bare host widens.
            return is_bare_host(&address)
                .then(|| newest_on_host(&noted, &host_of(&address)))
                .flatten();
        }
        let lower = asked.to_lowercase();
        let by_title = noted.iter().rev().find(|n| {
            !n.title.is_empty()
                && (n.title == lower
                    || (lower.chars().count() >= MIN_TITLE_PART_CHARS && n.title.contains(&lower)))
        });
        if let Some(n) = by_title {
            return Some(n.address.clone());
        }
        // A source line copied whole, such as "Timetable (krc.co.ke)": the host it names.
        addresses_in(asked)
            .iter()
            .filter(|a| is_bare_host(a))
            .find_map(|a| newest_on_host(&noted, &host_of(a)))
    }
}

/// The addresses the pond's own turns note into, shared by the chat, the runner and the tool.
pub fn global() -> &'static PageAddresses {
    static ADDRESSES: PageAddresses = PageAddresses::new();
    &ADDRESSES
}

/// The addresses written in `text`: whole `http(s)` links, and bare host names with or without a
/// path, such as `nation.africa/news`. Each normalized, in order, without repeats.
pub fn addresses_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        if found.len() == MAX_ADDRESSES_PER_TEXT {
            break;
        }
        let word = word
            .trim_start_matches(|c: char| "<(['\"`".contains(c))
            .trim_end_matches(|c: char| ".,;:!?)]}>'\"`".contains(c));
        if let Some(address) = normalize(word).filter(|a| !found.contains(a)) {
            found.push(address);
        }
    }
    found
}

/// `raw` as one address: `http(s)` only, a bare host read as `https`, scheme and host lowercased,
/// no fragment. `None` for anything else, and for one carrying a user name or password.
pub fn normalize(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_ADDRESS_BYTES || raw.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        raw.to_string()
    } else if looks_like_host(raw.split('/').next().unwrap_or_default()) {
        format!("https://{raw}")
    } else {
        return None;
    };
    let mut url = url::Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

/// `word` as a host name: dotted labels of letters, digits and dashes, ending in letters.
fn looks_like_host(word: &str) -> bool {
    let word = word.split(':').next().unwrap_or_default();
    let labels: Vec<&str> = word.split('.').collect();
    let Some(tld) = labels.last() else {
        return false;
    };
    labels.len() >= 2
        && (2..=24).contains(&tld.len())
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && labels.iter().all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// What two spellings of one page share: host without `www.`, path without a trailing slash,
/// and the query. The scheme is left out, so `http` and `https` name the same page.
fn key_of(address: &str) -> String {
    let Ok(url) = url::Url::parse(address) else {
        return address.to_string();
    };
    let path = url.path().trim_end_matches('/');
    match url.query() {
        Some(q) => format!("{}{path}?{q}", host_of(address)),
        None => format!("{}{path}", host_of(address)),
    }
}

fn host_of(address: &str) -> String {
    url::Url::parse(address)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .map(|h| h.strip_prefix("www.").map(str::to_string).unwrap_or(h))
        .unwrap_or_default()
}

fn is_bare_host(address: &str) -> bool {
    url::Url::parse(address).is_ok_and(|u| u.path() == "/" && u.query().is_none())
}

fn newest_on_host(noted: &[Noted], host: &str) -> Option<String> {
    noted
        .iter()
        .rev()
        .find(|n| n.host == host)
        .map(|n| n.address.clone())
}

fn prune(noted: &mut Vec<Noted>, now: Instant) {
    noted.retain(|n| now.saturating_duration_since(n.at) < ADDRESS_LIFETIME);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persons_links_and_bare_hosts_are_found() {
        let found = addresses_in(
            "Summarise https://nation.africa/kenya/news?id=4#top, then (see krc.co.ke/sgr). \
             Not e.g. this, not 3.5 hours, not me@example.com.",
        );
        assert_eq!(
            found,
            vec![
                "https://nation.africa/kenya/news?id=4".to_string(),
                "https://krc.co.ke/sgr".to_string(),
            ]
        );
    }

    #[test]
    fn credentials_and_other_schemes_are_never_addresses() {
        assert_eq!(normalize("https://user:pw@example.com/"), None);
        assert_eq!(normalize("ftp://example.com/file"), None);
        assert_eq!(normalize("javascript:alert(1)"), None);
        assert_eq!(normalize("https://"), None);
        assert_eq!(
            normalize("HTTPS://Example.COM/A#b").as_deref(),
            Some("https://example.com/A")
        );
    }

    #[test]
    fn only_a_noted_address_resolves() {
        let book = PageAddresses::new();
        let now = Instant::now();
        book.note_words_at("Read https://nation.africa/kenya/news/story-1 please", now);
        assert_eq!(
            book.resolve_at("https://nation.africa/kenya/news/story-1/", now)
                .as_deref(),
            Some("https://nation.africa/kenya/news/story-1"),
            "a trailing slash names the same page"
        );
        assert_eq!(
            book.resolve_at("nation.africa", now).as_deref(),
            Some("https://nation.africa/kenya/news/story-1"),
            "a bare host is the newest page noted there"
        );
        assert_eq!(
            book.resolve_at(
                "https://nation.africa/kenya/news/story-1?memory=wifi-password",
                now
            ),
            None,
            "a query nobody gave could carry data"
        );
        assert_eq!(book.resolve_at("https://evil.example/collect", now), None);
        assert_eq!(book.resolve_at("evil.example", now), None);
    }

    #[test]
    fn a_search_source_resolves_by_host_title_or_source_line() {
        let book = PageAddresses::new();
        let now = Instant::now();
        book.note_at(
            "https://www.krc.co.ke/sgr/timetable",
            "Madaraka Express timetable and fares",
            now,
        );
        let page = Some("https://www.krc.co.ke/sgr/timetable");
        assert_eq!(book.resolve_at("krc.co.ke", now).as_deref(), page);
        assert_eq!(book.resolve_at("www.krc.co.ke", now).as_deref(), page);
        assert_eq!(
            book.resolve_at("Madaraka Express timetable", now)
                .as_deref(),
            page
        );
        assert_eq!(
            book.resolve_at("Madaraka Express timetable and fares (krc.co.ke)", now)
                .as_deref(),
            page
        );
        assert_eq!(
            book.resolve_at("Express", now),
            None,
            "too little to pick by"
        );
    }

    #[test]
    fn an_address_is_forgotten_after_its_lifetime() {
        let book = PageAddresses::new();
        let then = Instant::now();
        book.note_words_at("https://example.com/a", then);
        let later = then + ADDRESS_LIFETIME + Duration::from_secs(1);
        assert_eq!(book.resolve_at("https://example.com/a", later), None);
    }

    #[test]
    fn the_book_keeps_only_the_newest() {
        let book = PageAddresses::new();
        let now = Instant::now();
        for i in 0..(MAX_ADDRESSES + 10) {
            book.note_at(&format!("https://example.com/{i}"), "", now);
        }
        assert_eq!(book.resolve_at("https://example.com/0", now), None);
        assert!(book
            .resolve_at(&format!("https://example.com/{}", MAX_ADDRESSES + 9), now)
            .is_some());
    }

    #[test]
    fn a_later_note_keeps_an_earlier_title() {
        let book = PageAddresses::new();
        let now = Instant::now();
        book.note_at("https://example.com/fares", "Fares and timetables", now);
        book.note_at("https://example.com/fares", "", now);
        assert_eq!(
            book.resolve_at("fares and timetables", now).as_deref(),
            Some("https://example.com/fares")
        );
    }
}
