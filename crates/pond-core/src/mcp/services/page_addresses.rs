//! The addresses `read_web_page` may fetch: one the person wrote, one a long task's goal names, a
//! search result, or a link or GET form on a page read from these, on that page's own site and
//! within two clicks. A model may pass the tool any string, and an address it wrote could carry
//! the household's data to whoever owns the host, so only an address one of these gave, or a host
//! one of them names, is read. Held in memory only.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a noted address stays readable: longer than a long task usually runs.
pub const ADDRESS_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

/// Addresses held at most; the oldest go first. Room for a few listing pages' links beside
/// what the person and the searches gave.
pub const MAX_ADDRESSES: usize = 512;

/// Addresses noted from one text at most.
const MAX_ADDRESSES_PER_TEXT: usize = 16;

/// Past this, an address is more likely data than a page.
const MAX_ADDRESS_BYTES: usize = 2_048;

/// A title shorter than this names too little to pick a page by part of it.
const MIN_TITLE_PART_CHARS: usize = 12;

/// Clicks from an address the person, a goal or a search gave: a listing, then its detail page.
pub const MAX_LINK_DEPTH: u8 = 2;

/// Search forms held at most; the oldest go first.
const MAX_FORMS: usize = 64;

/// A form's free-text value at most: a search, not a place to carry data.
pub const MAX_FORM_VALUE_CHARS: usize = 200;

#[derive(Debug, Clone)]
struct Noted {
    address: String,
    key: String,
    host: String,
    /// A search source's title, lowercased; empty for an address the person wrote.
    title: String,
    /// Clicks from an address given outright, which is 0.
    depth: u8,
    at: Instant,
}

/// A field of a page's GET form, as the page wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormField {
    /// Any short text, such as a search box or a price.
    Text(String),
    /// One of the options the page lists.
    Choice { name: String, options: Vec<String> },
    /// A value the page set itself and the reader may not change.
    Fixed { name: String, value: String },
}

impl FormField {
    pub fn name(&self) -> &str {
        match self {
            FormField::Text(name) => name,
            FormField::Choice { name, .. } | FormField::Fixed { name, .. } => name,
        }
    }

    fn admits(&self, value: &str) -> bool {
        match self {
            FormField::Text(_) => value.chars().count() <= MAX_FORM_VALUE_CHARS,
            FormField::Choice { options, .. } => options.iter().any(|o| o == value),
            FormField::Fixed { value: fixed, .. } => fixed == value,
        }
    }
}

/// A page's GET form: where it sends, and the fields it sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchForm {
    /// The address it sends to, without a query.
    pub action: String,
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone)]
struct NotedForm {
    /// Host and path of the action, as `key_of` writes them.
    key: String,
    form: SearchForm,
    /// The depth of the page the form sits on.
    depth: u8,
    at: Instant,
}

impl NotedForm {
    /// Whether `address` is this form sent: its action, with only its fields, each admitted.
    fn sends(&self, address: &url::Url) -> bool {
        let pairs: Vec<(String, String)> = address.query_pairs().into_owned().collect();
        !pairs.is_empty()
            && pairs.iter().all(|(name, value)| {
                self.form
                    .fields
                    .iter()
                    .any(|f| f.name() == name && f.admits(value))
            })
    }
}

/// The addresses a page read may fetch, newest last.
#[derive(Debug, Default)]
pub struct PageAddresses {
    noted: Mutex<Vec<Noted>>,
    forms: Mutex<Vec<NotedForm>>,
}

impl PageAddresses {
    pub const fn new() -> Self {
        Self {
            noted: Mutex::new(Vec::new()),
            forms: Mutex::new(Vec::new()),
        }
    }

    /// The address `asked` names, with its depth: as [`Self::resolve`], or a GET form a read page
    /// carries, sent with its own fields only.
    pub fn resolve_with_depth(&self, asked: &str) -> Option<(String, u8)> {
        self.resolve_with_depth_at(asked, Instant::now())
    }

    /// Notes the links of a page read at `page_depth`: those on its own site, one click deeper,
    /// while that stays within [`MAX_LINK_DEPTH`]. Returns how many were noted.
    pub fn note_links_from(&self, page: &str, page_depth: u8, links: &[(String, String)]) -> usize {
        let depth = page_depth.saturating_add(1);
        if depth > MAX_LINK_DEPTH {
            return 0;
        }
        let host = host_of(page);
        let now = Instant::now();
        let mut noted = 0;
        for (address, title) in links {
            if normalize(address).is_some_and(|a| host_of(&a) == host) {
                self.note_at_depth(address, title, depth, now);
                noted += 1;
            }
        }
        noted
    }

    /// Notes the GET forms of a page read at `page_depth` that send to its own site, while the
    /// results they give stay within [`MAX_LINK_DEPTH`]. Returns how many were noted.
    pub fn note_forms_from(&self, page: &str, page_depth: u8, forms: Vec<SearchForm>) -> usize {
        if page_depth.saturating_add(1) > MAX_LINK_DEPTH {
            return 0;
        }
        let host = host_of(page);
        let now = Instant::now();
        let mut held = self.forms.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|f| now.saturating_duration_since(f.at) < ADDRESS_LIFETIME);
        let mut noted = 0;
        for form in forms {
            let Some(action) = normalize(&form.action) else {
                continue;
            };
            if host_of(&action) != host || form.fields.is_empty() {
                continue;
            }
            let key = action_key(&action);
            held.retain(|f| f.key != key);
            held.push(NotedForm {
                key,
                form,
                depth: page_depth,
                at: now,
            });
            noted += 1;
        }
        let excess = held.len().saturating_sub(MAX_FORMS);
        held.drain(..excess);
        noted
    }

    /// Notes every address in the person's own words; returns how many.
    pub fn note_words(&self, text: &str) -> usize {
        self.note_words_at(text, Instant::now())
    }

    /// Notes a search result's address under its title.
    pub fn note_source(&self, address: &str, title: &str) {
        self.note_at(address, title, Instant::now());
    }

    /// Notes where a read at `depth` ended, after any redirect, so it may be read again by name.
    pub fn note_read(&self, address: &str, title: &str, depth: u8) {
        self.note_at_depth(address, title, depth, Instant::now());
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
        self.note_at_depth(address, title, 0, now);
    }

    fn note_at_depth(&self, address: &str, title: &str, depth: u8, now: Instant) {
        let Some(address) = normalize(address) else {
            return;
        };
        let key = key_of(&address);
        let host = host_of(&address);
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut noted = self.noted.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut noted, now);
        let (title, depth) = match noted.iter().position(|n| n.key == key) {
            Some(i) => {
                let old = noted.remove(i);
                let title = if title.is_empty() {
                    old.title
                } else {
                    title.to_lowercase()
                };
                // Reached again by a longer way, a page keeps its shortest.
                (title, depth.min(old.depth))
            }
            None => (title.to_lowercase(), depth),
        };
        noted.push(Noted {
            address,
            key,
            host,
            title,
            depth,
            at: now,
        });
        let excess = noted.len().saturating_sub(MAX_ADDRESSES);
        noted.drain(..excess);
    }

    fn resolve_at(&self, asked: &str, now: Instant) -> Option<String> {
        self.resolve_with_depth_at(asked, now).map(|(a, _)| a)
    }

    fn resolve_with_depth_at(&self, asked: &str, now: Instant) -> Option<(String, u8)> {
        let asked = asked
            .trim()
            .trim_matches(|c: char| "<>\"'`()[]".contains(c))
            .trim();
        if asked.is_empty() {
            return None;
        }
        let found = |n: &Noted| (n.address.clone(), n.depth);
        let mut noted = self.noted.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut noted, now);
        if let Some(address) = normalize(asked) {
            let key = key_of(&address);
            if let Some(n) = noted.iter().rev().find(|n| n.key == key) {
                return Some(found(n));
            }
            if let Some(depth) = self.sent_by_a_form(&address, now) {
                return Some((address, depth));
            }
            // A path or query nobody gave could be carrying data, so only a bare host widens.
            return is_bare_host(&address)
                .then(|| newest_on_host(&noted, &host_of(&address)).map(found))
                .flatten();
        }
        let lower = asked.to_lowercase();
        let by_title = noted.iter().rev().find(|n| {
            !n.title.is_empty()
                && (n.title == lower
                    || (lower.chars().count() >= MIN_TITLE_PART_CHARS && n.title.contains(&lower)))
        });
        if let Some(n) = by_title {
            return Some(found(n));
        }
        // A source line copied whole, such as "Timetable (krc.co.ke)": the host it names.
        addresses_in(asked)
            .iter()
            .filter(|a| is_bare_host(a))
            .find_map(|a| newest_on_host(&noted, &host_of(a)).map(found))
    }

    /// The depth of the results `address` is, when it is a noted form sent with its own fields.
    fn sent_by_a_form(&self, address: &str, now: Instant) -> Option<u8> {
        let url = url::Url::parse(address).ok()?;
        let key = action_key(address);
        let mut forms = self.forms.lock().unwrap_or_else(|e| e.into_inner());
        forms.retain(|f| now.saturating_duration_since(f.at) < ADDRESS_LIFETIME);
        forms
            .iter()
            .rev()
            .find(|f| f.key == key && f.sends(&url))
            .map(|f| f.depth.saturating_add(1))
    }
}

/// Whether two addresses are on one site: the same host, `www.` aside.
pub fn same_site(a: &str, b: &str) -> bool {
    let host = host_of(a);
    !host.is_empty() && host == host_of(b)
}

/// Host and path of a form's action, the query left out: the form writes its own.
fn action_key(address: &str) -> String {
    let Ok(url) = url::Url::parse(address) else {
        return address.to_string();
    };
    format!("{}{}", host_of(address), url.path().trim_end_matches('/'))
}

fn newest_on_host<'a>(noted: &'a [Noted], host: &str) -> Option<&'a Noted> {
    noted.iter().rev().find(|n| n.host == host)
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

    fn link(address: &str, text: &str) -> (String, String) {
        (address.to_string(), text.to_string())
    }

    #[test]
    fn a_read_pages_own_links_open_two_clicks_deep() {
        let book = PageAddresses::new();
        book.note_words("look at https://cars.example/used");
        let (listing, depth) = book
            .resolve_with_depth("https://cars.example/used")
            .unwrap();
        assert_eq!(depth, 0);
        let noted = book.note_links_from(
            &listing,
            depth,
            &[
                link("https://www.cars.example/used/123", "2017 Forester"),
                link("https://elsewhere.example/ad", "An offer"),
            ],
        );
        assert_eq!(noted, 1, "another site's link is not followed");
        assert_eq!(
            book.resolve_with_depth("https://cars.example/used/123"),
            Some(("https://www.cars.example/used/123".to_string(), 1))
        );
        assert_eq!(book.resolve("https://elsewhere.example/ad"), None);
        book.note_links_from(
            "https://cars.example/used/123",
            1,
            &[link("https://cars.example/dealer/9", "Dealer")],
        );
        assert_eq!(
            book.resolve_with_depth("https://cars.example/dealer/9")
                .map(|(_, d)| d),
            Some(2)
        );
        let past = book.note_links_from(
            "https://cars.example/dealer/9",
            2,
            &[link("https://cars.example/more", "More")],
        );
        assert_eq!(past, 0, "a third click is not noted");
        assert_eq!(book.resolve("https://cars.example/more"), None);
    }

    #[test]
    fn a_page_reached_again_keeps_its_shortest_depth() {
        let book = PageAddresses::new();
        book.note_links_from("https://a.example/", 1, &[link("https://a.example/x", "X")]);
        book.note_read("https://a.example/x", "X", 0);
        assert_eq!(
            book.resolve_with_depth("https://a.example/x")
                .map(|(_, d)| d),
            Some(0)
        );
    }

    fn search_form() -> SearchForm {
        SearchForm {
            action: "https://cars.example/search".into(),
            fields: vec![
                FormField::Text("q".into()),
                FormField::Choice {
                    name: "make".into(),
                    options: vec!["subaru".into(), "toyota".into()],
                },
                FormField::Fixed {
                    name: "kind".into(),
                    value: "used".into(),
                },
            ],
        }
    }

    #[test]
    fn a_get_form_is_sent_with_its_own_fields_only() {
        let book = PageAddresses::new();
        assert_eq!(
            book.note_forms_from("https://cars.example/", 0, vec![search_form()]),
            1
        );
        assert_eq!(
            book.resolve_with_depth("https://cars.example/search?make=subaru&q=forester&kind=used"),
            Some((
                "https://cars.example/search?make=subaru&q=forester&kind=used".to_string(),
                1
            ))
        );
        for refused in [
            "https://cars.example/search?make=ferrari",
            "https://cars.example/search?kind=new",
            "https://cars.example/search?q=x&token=abc",
            "https://cars.example/search",
            "https://cars.example/other?q=x",
        ] {
            assert_eq!(book.resolve(refused), None, "{refused}");
        }
        let long = "x".repeat(MAX_FORM_VALUE_CHARS + 1);
        assert_eq!(
            book.resolve(&format!("https://cars.example/search?q={long}")),
            None
        );
    }

    #[test]
    fn a_form_sending_elsewhere_or_from_too_deep_is_not_noted() {
        let book = PageAddresses::new();
        let mut away = search_form();
        away.action = "https://collector.example/search".into();
        assert_eq!(
            book.note_forms_from("https://cars.example/", 0, vec![away]),
            0
        );
        assert_eq!(
            book.note_forms_from("https://cars.example/a/b", 2, vec![search_form()]),
            0
        );
        assert_eq!(book.resolve("https://cars.example/search?q=x"), None);
    }

    #[test]
    fn same_site_ignores_www_only() {
        assert!(same_site("https://www.a.example/x", "http://a.example/y"));
        assert!(!same_site("https://m.a.example/", "https://a.example/"));
        assert!(!same_site("not an address", "not an address"));
    }
}
