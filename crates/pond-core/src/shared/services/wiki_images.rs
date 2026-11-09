//! A report's picture from Wikipedia: an article's lead image, with the licence, author and file
//! page its credit needs. A non-free image (fair use on one wiki) is never taken, since the pond
//! keeps and serves its own copy.

use serde_json::Value;

/// The width the pond keeps: enough for a phone's card, small enough to store.
pub const IMAGE_WIDTH: u32 = 800;

/// One image a report may show, with its credit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditedImage {
    /// The scaled copy to fetch.
    pub url: String,
    /// The file's own page, where its licence is stated in full.
    pub file_page: String,
    pub licence: String,
    /// The author in plain text, markup removed.
    pub author: Option<String>,
    pub caption: Option<String>,
}

/// The query for an article's lead image file name, in `lang`'s Wikipedia.
pub fn lead_image_query(lang: &str, title: &str) -> String {
    format!(
        "https://{lang}.wikipedia.org/w/api.php?action=query&format=json&formatversion=2\
         &prop=pageimages&piprop=name&titles={}",
        encode(title)
    )
}

/// The file name the lead-image answer names, if the article has one.
pub fn lead_image_name(answer: &Value) -> Option<String> {
    answer["query"]["pages"]
        .as_array()?
        .iter()
        .find_map(|p| p["pageimage"].as_str())
        .map(str::to_string)
}

/// The query for a file's scaled copy and its licence metadata.
pub fn image_info_query(lang: &str, file: &str) -> String {
    format!(
        "https://{lang}.wikipedia.org/w/api.php?action=query&format=json&formatversion=2\
         &prop=imageinfo&iiprop=url|extmetadata&iiurlwidth={IMAGE_WIDTH}&titles=File:{}",
        encode(file)
    )
}

/// The image the info answer describes, unless it is non-free or names no licence.
pub fn credited_image(answer: &Value) -> Option<CreditedImage> {
    let info = answer["query"]["pages"]
        .as_array()?
        .iter()
        .find_map(|p| p["imageinfo"].as_array().and_then(|i| i.first()))?;
    let meta = &info["extmetadata"];
    let field = |key: &str| {
        meta[key]["value"]
            .as_str()
            .map(plain)
            .filter(|v| !v.is_empty())
    };
    if field("NonFree").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return None;
    }
    let licence = field("LicenseShortName")?;
    if licence.to_lowercase().contains("fair use") {
        return None;
    }
    let url = info["thumburl"]
        .as_str()
        .or_else(|| info["url"].as_str())?
        .to_string();
    let file_page = info["descriptionurl"].as_str()?.to_string();
    Some(CreditedImage {
        url,
        file_page,
        licence,
        author: field("Artist"),
        caption: field("ImageDescription").map(|c| clip(&c, 200)),
    })
}

/// `text` as a query value, spaces as `%20` the way the wiki's own links write them.
fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// The language and article title a Wikipedia address names: `en.wikipedia.org/wiki/Fort_Jesus`
/// is ("en", "Fort Jesus").
pub fn article_of(address: &str) -> Option<(String, String)> {
    let url = url::Url::parse(address).ok()?;
    let host = url.host_str()?;
    let lang = host
        .strip_suffix(".wikipedia.org")
        .map(|l| l.trim_start_matches("m.").trim_end_matches(".m"))?;
    if lang.is_empty() || !lang.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    let title = url.path().strip_prefix("/wiki/")?;
    let title = percent_decode(title).replace('_', " ");
    (!title.is_empty()).then(|| (lang.to_string(), title))
}

fn percent_decode(text: &str) -> String {
    url::form_urlencoded::parse(format!("t={}", text.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| text.to_string())
}

/// `html` as plain text: tags dropped, entities for the common characters read, space folded.
fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}...", text[..at].trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_free_image_comes_with_its_credit() {
        let answer = json!({"query": {"pages": [{"imageinfo": [{
            "thumburl": "https://upload.wikimedia.org/thumb/800px-Fort.jpg",
            "descriptionurl": "https://commons.wikimedia.org/wiki/File:Fort.jpg",
            "extmetadata": {
                "LicenseShortName": {"value": "CC BY-SA 4.0"},
                "Artist": {"value": "<a href=\"//commons.wikimedia.org/wiki/User:Ann\">Ann &amp; Bo</a>"},
                "ImageDescription": {"value": "Fort Jesus, <i>Mombasa</i>"}
            }
        }]}]}});
        let image = credited_image(&answer).unwrap();
        assert_eq!(
            image.url,
            "https://upload.wikimedia.org/thumb/800px-Fort.jpg"
        );
        assert_eq!(image.licence, "CC BY-SA 4.0");
        assert_eq!(image.author.as_deref(), Some("Ann & Bo"));
        assert_eq!(image.caption.as_deref(), Some("Fort Jesus, Mombasa"));
    }

    #[test]
    fn a_non_free_or_unlicensed_image_is_never_taken() {
        let with = |meta: Value| {
            json!({"query": {"pages": [{"imageinfo": [{
                "thumburl": "https://x/y.jpg", "descriptionurl": "https://x/File:y.jpg",
                "extmetadata": meta
            }]}]}})
        };
        assert!(credited_image(&with(
            json!({"NonFree": {"value": "true"}, "LicenseShortName": {"value": "Fair use"}})
        ))
        .is_none());
        assert!(
            credited_image(&with(json!({"LicenseShortName": {"value": "Fair use"}}))).is_none()
        );
        assert!(credited_image(&with(json!({}))).is_none());
    }

    #[test]
    fn the_lead_image_is_named_by_the_article() {
        let answer =
            json!({"query": {"pages": [{"title": "Fort Jesus", "pageimage": "Fort.jpg"}]}});
        assert_eq!(lead_image_name(&answer).as_deref(), Some("Fort.jpg"));
        assert!(lead_image_query("en", "Fort Jesus").ends_with("titles=Fort%20Jesus"));
        assert_eq!(
            article_of("https://en.wikipedia.org/wiki/Fort_Jesus"),
            Some(("en".to_string(), "Fort Jesus".to_string()))
        );
        assert_eq!(
            article_of("https://sw.m.wikipedia.org/wiki/Mombasa%20Old%20Town"),
            Some(("sw".to_string(), "Mombasa Old Town".to_string())),
            "a mobile host names its language before .m"
        );
        assert_eq!(article_of("https://example.org/wiki/Fort"), None);
        assert!(image_info_query("en", "Fort.jpg").contains("iiurlwidth=800"));
    }
}
