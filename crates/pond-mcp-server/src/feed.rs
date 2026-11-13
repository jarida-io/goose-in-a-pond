//! Reading a feed (RSS or Atom) as a page. A feed is turned into a small HTML document of
//! `<article>` entries, the shape the page reader already knows, so the page tool, watches and
//! the news read a feed with no path of their own.

use quick_xml::events::Event;
use quick_xml::Reader;

/// Entries read from one feed.
const MAX_ENTRIES: usize = 60;
/// The most of an entry's summary that is kept, in characters.
const SUMMARY_CHARS: usize = 400;

/// Media types a feed is served as.
pub const FEED_TYPES: [&str; 5] = [
    "application/rss+xml",
    "application/atom+xml",
    "application/xml",
    "text/xml",
    "application/rdf+xml",
];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Entry {
    title: String,
    link: String,
    summary: String,
    when: String,
}

fn local(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    name.rsplit(':').next().unwrap_or_default().to_lowercase()
}

/// `text` with its markup taken out: a summary in a feed is often HTML.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The feed's title and entries, newest as the feed gives them; `None` when `xml` is not a feed.
fn entries(xml: &str) -> Option<(String, Vec<Entry>)> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut text: Vec<String> = Vec::new();
    let mut feed_title = String::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut current: Option<Entry> = None;
    let mut is_feed = false;

    // An Atom link is an attribute: the page for people (`alternate`, or no `rel`) over others.
    let atom_link = |e: &quick_xml::events::BytesStart<'_>| -> Option<String> {
        let mut href = None;
        let mut rel = String::new();
        for attr in e.attributes().flatten() {
            let value = String::from_utf8_lossy(&attr.value).to_string();
            match local(attr.key.as_ref()).as_str() {
                "href" => href = Some(value),
                "rel" => rel = value,
                _ => {}
            }
        }
        href.filter(|_| rel.is_empty() || rel == "alternate")
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref());
                if stack.is_empty() {
                    is_feed = matches!(name.as_str(), "rss" | "feed" | "rdf");
                    if !is_feed {
                        return None;
                    }
                }
                if matches!(name.as_str(), "item" | "entry") {
                    current = Some(Entry::default());
                }
                if name == "link" {
                    if let (Some(entry), Some(href)) = (current.as_mut(), atom_link(&e)) {
                        if entry.link.is_empty() {
                            entry.link = href;
                        }
                    }
                }
                stack.push(name);
                text.push(String::new());
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "link" {
                    if let (Some(entry), Some(href)) = (current.as_mut(), atom_link(&e)) {
                        if entry.link.is_empty() {
                            entry.link = href;
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let (Some(top), Ok(words)) = (text.last_mut(), e.decode()) {
                    top.push_str(&words);
                }
            }
            Ok(Event::CData(e)) => {
                if let (Some(top), Ok(words)) = (text.last_mut(), e.decode()) {
                    top.push_str(&words);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let Some(top) = text.last_mut() {
                    if let Ok(Some(ch)) = e.resolve_char_ref() {
                        top.push(ch);
                    } else if let Ok(name) = e.decode() {
                        match quick_xml::escape::resolve_predefined_entity(&name) {
                            Some(value) => top.push_str(value),
                            None => top.push(' '),
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = local(e.name().as_ref());
                let words = text.pop().unwrap_or_default();
                let words = words.trim();
                stack.pop();
                match current.as_mut() {
                    Some(entry) => match name.as_str() {
                        "title" if entry.title.is_empty() => entry.title = plain(words),
                        "link" if entry.link.is_empty() && !words.is_empty() => {
                            entry.link = words.to_string()
                        }
                        // A permalink guid stands in for a missing link.
                        "guid" | "id" if entry.link.is_empty() && words.starts_with("http") => {
                            entry.link = words.to_string()
                        }
                        "description" | "summary" if entry.summary.is_empty() => {
                            entry.summary = plain(words).chars().take(SUMMARY_CHARS).collect()
                        }
                        "content" | "encoded" if entry.summary.is_empty() => {
                            entry.summary = plain(words).chars().take(SUMMARY_CHARS).collect()
                        }
                        "pubdate" | "published" | "updated" | "date" if entry.when.is_empty() => {
                            entry.when = words.to_string()
                        }
                        "item" | "entry" => {
                            if let Some(done) = current.take().filter(|e| !e.title.is_empty()) {
                                entries.push(done);
                                if entries.len() >= MAX_ENTRIES {
                                    break;
                                }
                            }
                        }
                        _ => {}
                    },
                    // The feed's own title sits one level under `channel` or `feed`.
                    None if name == "title" && feed_title.is_empty() && stack.len() <= 2 => {
                        feed_title = plain(words)
                    }
                    None => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    is_feed.then_some((feed_title, entries))
}

/// The feed as an HTML page of articles, or `None` when `xml` is not a feed.
pub fn as_html(xml: &str) -> Option<String> {
    let (title, entries) = entries(xml)?;
    let mut html = format!(
        "<html><head><title>{}</title></head><body><main>",
        escaped(&title)
    );
    for entry in &entries {
        html.push_str("<article><h2>");
        match entry.link.is_empty() {
            true => html.push_str(&escaped(&entry.title)),
            false => html.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escaped(&entry.link),
                escaped(&entry.title)
            )),
        }
        html.push_str("</h2>");
        if !entry.when.is_empty() {
            html.push_str(&format!(
                "<time datetime=\"{0}\">{0}</time>",
                escaped(&entry.when)
            ));
        }
        if !entry.summary.is_empty() {
            html.push_str(&format!("<p>{}</p>", escaped(&entry.summary)));
        }
        html.push_str("</article>");
    }
    html.push_str("</main></body></html>");
    Some(html)
}

/// Whether a body served as `mime` should be tried as a feed.
pub fn may_be_feed(mime: &str) -> bool {
    FEED_TYPES.contains(&mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>School &amp; Parents Blog</title>
    <link>https://school.example/blog</link>
    <item>
      <title>Term dates for 2027</title>
      <link>https://school.example/blog/term-dates</link>
      <description><![CDATA[<p>The first term opens on <b>5 January</b>.</p>]]></description>
      <pubDate>Thu, 12 Nov 2026 08:00:00 +0300</pubDate>
    </item>
    <item>
      <title>Sports day moved</title>
      <guid isPermaLink="true">https://school.example/blog/sports-day</guid>
      <description>It is now on the 20th &amp; starts at nine.</description>
    </item>
    <item><description>No title, so not an entry</description></item>
  </channel>
</rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Releases</title>
  <link rel="self" href="https://code.example/releases.atom"/>
  <entry>
    <title>v2.4.0</title>
    <link rel="alternate" type="text/html" href="https://code.example/releases/v2.4.0"/>
    <link rel="enclosure" href="https://code.example/v2.4.0.zip"/>
    <updated>2026-11-10T10:00:00Z</updated>
    <content type="html">&lt;p&gt;Fixes the &lt;i&gt;pairing&lt;/i&gt; bug.&lt;/p&gt;</content>
  </entry>
</feed>"#;

    #[test]
    fn an_rss_feed_is_its_entries_with_links_dates_and_plain_summaries() {
        let (title, entries) = entries(RSS).unwrap();
        assert_eq!(title, "School & Parents Blog");
        assert_eq!(entries.len(), 2, "an item with no title is not an entry");
        assert_eq!(entries[0].title, "Term dates for 2027");
        assert_eq!(entries[0].link, "https://school.example/blog/term-dates");
        assert_eq!(entries[0].summary, "The first term opens on 5 January .");
        assert_eq!(entries[0].when, "Thu, 12 Nov 2026 08:00:00 +0300");
        assert_eq!(
            entries[1].link, "https://school.example/blog/sports-day",
            "a permalink guid stands in for a missing link"
        );
        assert_eq!(
            entries[1].summary,
            "It is now on the 20th & starts at nine."
        );
    }

    #[test]
    fn an_atom_feed_takes_the_link_for_people_not_the_download() {
        let (title, entries) = entries(ATOM).unwrap();
        assert_eq!(title, "Releases");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "v2.4.0");
        assert_eq!(entries[0].link, "https://code.example/releases/v2.4.0");
        assert_eq!(entries[0].summary, "Fixes the pairing bug.");
    }

    #[test]
    fn a_feed_becomes_a_page_of_articles_and_other_xml_does_not() {
        let html = as_html(RSS).unwrap();
        assert!(html.contains("<title>School &amp; Parents Blog</title>"));
        assert!(html.contains(
            "<article><h2><a href=\"https://school.example/blog/term-dates\">Term dates for 2027</a></h2>"
        ));
        assert!(html.contains("<p>It is now on the 20th &amp; starts at nine.</p>"));
        assert_eq!(
            as_html("<?xml version=\"1.0\"?><sitemap><url/></sitemap>"),
            None
        );
        assert_eq!(as_html("not xml at all"), None);
        assert!(may_be_feed("application/atom+xml"));
        assert!(!may_be_feed("application/pdf"));
    }
}
