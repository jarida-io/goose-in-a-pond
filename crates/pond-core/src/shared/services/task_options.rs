//! The options an exploring task found, read out of its compare step's Markdown so a screen can
//! show them as cards and a member can save or watch one. The compare step is told to write each
//! option as a `## ` heading over `- Field: value` lines; this reads that shape and tolerates the
//! drift a small model brings: a numbered heading, bold labels, a missing field.

use serde::Serialize;

use super::research_report::{sources, Source};

/// Options read from one text at most.
const MAX_OPTIONS: usize = 12;
/// Headings that hold the report's own parts, never an option.
const NOT_OPTIONS: &[&str] = &[
    "sources",
    "references",
    "summary",
    "overview",
    "notes",
    "conclusion",
];

/// Fields kept for one option beside the named ones.
const MAX_EXTRA_FIELDS: usize = 8;
const MAX_VALUE_CHARS: usize = 300;

/// One option the task found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TaskOption {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Why it fits what the person asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Other facts the step gave, such as mileage or location, in its order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Source", 2)?;
        s.serialize_field("title", &self.title)?;
        s.serialize_field("url", &self.url)?;
        s.end()
    }
}

/// The options in `markdown`: each `## ` heading followed by at least a price or a link among its
/// `- Field: value` lines. Headings that are not options (a summary, the sources) are skipped.
pub fn options_in(markdown: &str) -> Vec<TaskOption> {
    let mut options = Vec::new();
    let mut current: Option<(TaskOption, String)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed
            .strip_prefix("## ")
            .or_else(|| trimmed.strip_prefix("### "))
        {
            if let Some(done) = current.take() {
                options.extend(finished(done));
            }
            current = Some((
                TaskOption {
                    title: option_title(heading),
                    ..Default::default()
                },
                String::new(),
            ));
            continue;
        }
        if trimmed.starts_with("# ") {
            if let Some(done) = current.take() {
                options.extend(finished(done));
            }
            continue;
        }
        let Some((option, text)) = current.as_mut() else {
            continue;
        };
        text.push_str(line);
        text.push('\n');
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let Some((label, value)) = item.split_once(':') else {
            continue;
        };
        let label = label.trim().trim_matches('*').trim().to_string();
        let value = clip(value.trim().trim_matches('*').trim());
        // A bare link's own colon is not a label's.
        if label.is_empty() || value.is_empty() || label.contains('[') || label.contains("http") {
            continue;
        }
        match label.to_lowercase().as_str() {
            "price" | "cost" | "asking price" => option.price = Some(value),
            "link" | "url" | "listing" | "page" => {
                option.link = sources(&value)
                    .first()
                    .map(|s| s.url.clone())
                    .or_else(|| bare_link(&value));
            }
            "why" | "why it fits" | "fit" | "reason" => option.why = Some(value),
            "source" | "sources" => {}
            _ if option.fields.len() < MAX_EXTRA_FIELDS => option.fields.push((label, value)),
            _ => {}
        }
        if options.len() >= MAX_OPTIONS {
            break;
        }
    }
    if let Some(done) = current.take() {
        options.extend(finished(done));
    }
    options.truncate(MAX_OPTIONS);
    options
}

fn finished((mut option, text): (TaskOption, String)) -> Option<TaskOption> {
    option.sources = sources(&text);
    if option.link.is_none() {
        option.link = option.sources.first().map(|s| s.url.clone());
    }
    let named_part = NOT_OPTIONS.contains(&option.title.to_lowercase().as_str());
    let is_option = !option.title.is_empty()
        && !named_part
        && (option.price.is_some() || option.link.is_some());
    is_option.then_some(option)
}

/// "Option 2: 2017 Subaru Forester" and "2. 2017 Subaru Forester" are "2017 Subaru Forester".
fn option_title(heading: &str) -> String {
    let heading = heading.trim().trim_matches('*').trim();
    let lower = heading.to_lowercase();
    let rest = if lower.starts_with("option") {
        heading[6..]
            .trim_start()
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches([':', '.', '-', ')', ' '])
    } else {
        heading
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')', ' '])
    };
    let rest = rest.trim().trim_matches('*').trim();
    if rest.is_empty() { heading } else { rest }.to_string()
}

fn bare_link(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| "<>()[],.".contains(c)))
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(str::to_string)
}

fn clip(value: &str) -> String {
    match value.char_indices().nth(MAX_VALUE_CHARS) {
        Some((at, _)) => value[..at].trim_end().to_string(),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPARE: &str = "# Three cars that fit\n\nAll under 1.5M.\n\n\
        ## Option 1: 2017 Subaru Forester\n\
        - Price: KES 1,450,000\n\
        - Mileage: 98,000 km\n\
        - Link: [Forester on Cars](https://cars.example/used/2)\n\
        - Why: Lowest mileage of the three, one owner.\n\n\
        ## 2. **2016 Subaru Forester XT**\n\
        - **Price**: KES 1,400,000\n\
        - Location: Nairobi\n\
        - Source: [Dealer page](https://dealer.example/xt)\n\n\
        ## Sources\n\
        - [Forester on Cars](https://cars.example/used/2)\n";

    #[test]
    fn options_are_read_with_their_fields_and_sources() {
        let options = options_in(COMPARE);
        assert_eq!(options.len(), 2, "{options:?}");
        let first = &options[0];
        assert_eq!(first.title, "2017 Subaru Forester");
        assert_eq!(first.price.as_deref(), Some("KES 1,450,000"));
        assert_eq!(first.link.as_deref(), Some("https://cars.example/used/2"));
        assert_eq!(
            first.why.as_deref(),
            Some("Lowest mileage of the three, one owner.")
        );
        assert_eq!(
            first.fields,
            vec![("Mileage".to_string(), "98,000 km".to_string())]
        );
        let second = &options[1];
        assert_eq!(second.title, "2016 Subaru Forester XT");
        assert_eq!(second.price.as_deref(), Some("KES 1,400,000"));
        assert_eq!(
            second.link.as_deref(),
            Some("https://dealer.example/xt"),
            "from its source"
        );
        assert_eq!(
            second.fields,
            vec![("Location".to_string(), "Nairobi".to_string())]
        );
    }

    #[test]
    fn a_heading_without_a_price_or_link_is_not_an_option() {
        assert!(options_in("## Summary\nNothing fit.\n## Sources\n- none\n").is_empty());
        let bare = options_in("## Kettle\n- Link: https://shop.example/k1.\n");
        assert_eq!(bare[0].link.as_deref(), Some("https://shop.example/k1"));
    }
}
