//! The news prompts and what is taken from their answers: a story from one page, a digest
//! section from the day's stories, and the digest's title and opening.

use serde::Deserialize;
use serde_json::Value;

use crate::models::services::thought_filter::strip_reasoning;
use crate::shared::domain::news::{InterestRow, Portrait};

/// Page text a story prompt carries at most: about 1,500 tokens, so the Jetson's prefill stays
/// within seconds.
pub const PAGE_CHARS: usize = 6_000;
/// Page text a digest section's extra reading carries per page.
pub const EXTRA_PAGE_CHARS: usize = 2_500;
/// A page with less text than this is not worth a story.
pub const MIN_PAGE_CHARS: usize = 600;

/// A story's summary, in words: the prompt asks 120 to 250 and the check allows slack.
pub const STORY_WORDS: (usize, usize) = (70, 320);
/// A long read's, asked 300 to 500.
pub const LONG_READ_WORDS: (usize, usize) = (180, 650);
const MAX_HEADLINE_CHARS: usize = 120;
const MAX_DEK_CHARS: usize = 200;
const MAX_WHY_CHARS: usize = 200;
const MAX_HEADING_CHARS: usize = 90;
const MIN_BODY_WORDS: usize = 30;
const MAX_TITLE_CHARS: usize = 80;

/// What the background model is told about the member it writes for.
#[derive(Debug, Clone, Default)]
pub struct MemberContext {
    pub name: String,
    pub portrait: Option<Portrait>,
    /// The topics the news covers for them, for a member with no portrait yet.
    pub interests: Vec<String>,
}

impl MemberContext {
    /// What is known about the member, and the rule that keeps a passing question from reading
    /// as an interest.
    pub fn about(&self) -> String {
        let mut lines = vec![format!("You are writing for {}.", self.name)];
        if let Some(p) = &self.portrait {
            if !p.summary.trim().is_empty() {
                lines.push(format!(
                    "What the pond knows about them: {}",
                    p.summary.trim()
                ));
            }
            for (label, items) in [
                ("Into", &p.into),
                ("Working on", &p.working_on),
                ("Likes", &p.likes),
            ] {
                if !items.is_empty() {
                    lines.push(format!("{label}: {}.", items.join("; ")));
                }
            }
        }
        if !self.interests.is_empty() {
            lines.push(format!(
                "Interests they follow in the news: {}.",
                self.interests.join("; ")
            ));
        }
        lines.push(
            "One question asked once is curiosity, not an interest: lean only on what they \
             keep coming back to, and never invent anything about them."
                .to_string(),
        );
        lines.join("\n")
    }
}

/// The interest a story or a section is for, as the prompt names it.
pub fn interest_line(interest: &InterestRow) -> String {
    let mut line = format!("The interest: {}", interest.topic.trim());
    if !interest.facets.is_empty() {
        line.push_str(&format!(
            " (what they follow in it: {})",
            interest.facets.join(", ")
        ));
    }
    line.push_str(&format!(". They are {} about it.", interest.depth));
    line
}

pub const STORY_SYSTEM: &str = "\
You write one short news story for a member of a household, from one web page the pond read \
for them.

Write only from the page. Every name, number, date and claim must be on the page; leave out \
anything that is not. Add no background from your own knowledge. Use your own words and never \
copy a sentence from the page. Keep your thinking concise, grounded, and focused on extracting exact facts from the page.

Reply with JSON and nothing else:
{\"headline\": \"...\", \"dek\": \"...\", \"summary\": \"...\", \"why_you\": \"...\"}

- headline: what happened, specifically: name who or what, and the key fact or number. Under 90 \
characters. Not a question, no teaser, no colon.
- dek: one sentence under 140 characters with the next most important fact.
- summary: Markdown, 120 to 250 words, in two or three short paragraphs with no headings, the \
most important first: who, what, when, where, and what happens next if the page says.
- why_you: one sentence under 120 characters tying the story to one specific thing known about \
the member (a plan, a budget, a project, a date) and what it means for it. Address them as \
\"you\".

Date anything only with a date the page gives; today's date is for judging what is upcoming, \
never a date for the news.

If the page is not a news story or report about the interest (a home page, a list of links, a \
shop, a page with little text, or old news with nothing new), reply {\"skip\": \"<a few \
words>\"} instead.";

/// The story prompt's user message: the member, the interest, the day and the page.
pub fn story_prompt(
    member: &MemberContext,
    interest: &InterestRow,
    today: &str,
    page: &PageForPrompt<'_>,
) -> String {
    format!(
        "{about}\n\n{interest}\n\nToday is {today}.\n\nThe page:\nTitle: {title}\nSite: {site}\n\
         Published: {published}\nAddress: {url}\nText:\n{text}",
        about = member.about(),
        interest = interest_line(interest),
        title = page.title,
        site = page.site,
        published = page.published.unwrap_or("not stated"),
        url = page.url,
        text = page_text(page.lines, PAGE_CHARS),
    )
}

/// A page as a prompt quotes it.
pub struct PageForPrompt<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub site: &'a str,
    pub published: Option<&'a str>,
    pub lines: &'a [String],
}

/// The page's lines in order, within `max` characters.
pub fn page_text(lines: &[String], max: usize) -> String {
    let mut text = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if text.len() + line.len() + 1 > max {
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim_end().to_string()
}

/// How much text the page's lines hold.
pub fn text_chars(lines: &[String]) -> usize {
    lines.iter().map(|l| l.trim().len()).sum()
}

/// A story as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenStory {
    pub headline: String,
    pub dek: String,
    pub summary: String,
    pub why_you: String,
}

/// Why an answer gave no story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoryRefusal {
    /// The model said the page is not a story about the interest.
    Skipped(String),
    /// No JSON object, or not the fields asked for.
    Unparseable,
    /// A field outside its bounds.
    OutOfBounds(&'static str),
    /// It names a number the page does not carry.
    NotOnThePage(String),
}

#[derive(Deserialize)]
struct RawStory {
    #[serde(default)]
    headline: Option<String>,
    #[serde(default)]
    dek: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    why_you: Option<String>,
    #[serde(default)]
    skip: Option<Value>,
}

/// The story in `reply`, checked against the page it was written from.
pub fn parse_story(
    reply: &str,
    page_lines: &[String],
    today: &str,
) -> Result<WrittenStory, StoryRefusal> {
    parse_written(reply, page_lines, today, STORY_WORDS)
}

/// A story or long read in `reply` whose summary runs `words` (fewest, most).
pub fn parse_written(
    reply: &str,
    page_lines: &[String],
    today: &str,
    words: (usize, usize),
) -> Result<WrittenStory, StoryRefusal> {
    let object = json_object(reply).ok_or(StoryRefusal::Unparseable)?;
    let raw: RawStory =
        serde_json::from_value(Value::Object(object)).map_err(|_| StoryRefusal::Unparseable)?;
    if let Some(skip) = raw.skip.filter(|s| !s.is_null()) {
        let why = skip
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| skip.to_string());
        return Err(StoryRefusal::Skipped(why));
    }
    let headline = one_line(raw.headline.as_deref().unwrap_or_default());
    let headline = headline.trim_end_matches('.').to_string();
    let dek = one_line(raw.dek.as_deref().unwrap_or_default());
    let summary = raw.summary.unwrap_or_default().trim().to_string();
    let why_you = one_line(raw.why_you.as_deref().unwrap_or_default());
    if headline.is_empty() || dek.is_empty() || summary.is_empty() || why_you.is_empty() {
        return Err(StoryRefusal::Unparseable);
    }
    if headline.chars().count() > MAX_HEADLINE_CHARS {
        return Err(StoryRefusal::OutOfBounds("headline"));
    }
    if dek.chars().count() > MAX_DEK_CHARS {
        return Err(StoryRefusal::OutOfBounds("dek"));
    }
    if why_you.chars().count() > MAX_WHY_CHARS {
        return Err(StoryRefusal::OutOfBounds("why_you"));
    }
    let count = summary.split_whitespace().count();
    if !(words.0..=words.1).contains(&count) {
        return Err(StoryRefusal::OutOfBounds("summary"));
    }
    let haystack = number_haystack(page_lines, today);
    if let Some(n) = unknown_numbers(&headline, &haystack).into_iter().next() {
        return Err(StoryRefusal::NotOnThePage(n));
    }
    let unknown = unknown_numbers(&format!("{dek} {summary}"), &haystack);
    if unknown.len() > 1 {
        return Err(StoryRefusal::NotOnThePage(unknown.join(", ")));
    }
    Ok(WrittenStory {
        headline,
        dek,
        summary,
        why_you,
    })
}

pub const LONG_READ_SYSTEM: &str = "\
You write one long read for a member of a household: a deeper, evergreen piece about one subject \
they care about, from the numbered web pages the pond read for them.

Write only from the pages. Every name, number, date and claim must be on one of them; cite each \
with its page's number in square brackets right after it, like [2]. Add no background from your \
own knowledge. Use your own words and never copy a sentence. Keep your thinking concise, structured, and focused on synthesizing insights across the pages.

Reply with JSON and nothing else:
{\"headline\": \"...\", \"dek\": \"...\", \"summary\": \"...\", \"why_you\": \"...\"}

- headline: the subject and the angle, specifically, under 90 characters. Not a question, no \
teaser.
- dek: one sentence under 140 characters on what the piece explains.
- summary: Markdown, 300 to 500 words, in four to six short paragraphs: what it is, how it works \
or came about, where it stands now, and what is still open, as far as the pages say.
- why_you: one sentence under 120 characters on why this matters to the member, from what is \
known about them. Address them as \"you\".

If the pages do not explain the subject, reply {\"skip\": \"<a few words>\"} instead.";

/// The long read prompt's user message: the member, the subject, and the numbered pages.
pub fn long_read_prompt(
    member: &MemberContext,
    subject: &str,
    query: &str,
    today: &str,
    pages: &[PageForPrompt<'_>],
) -> String {
    let mut text = format!(
        "{}\n\nThe subject: {subject} (searched as \"{query}\").\n\nToday is {today}.\n\nPages:",
        member.about()
    );
    for (i, page) in pages.iter().enumerate() {
        text.push_str(&format!(
            "\n\n[{}] {} - {}\nAddress: {}\n{}",
            i + 1,
            page.site,
            page.title,
            page.url,
            page_text(page.lines, LONG_READ_PAGE_CHARS)
        ));
    }
    text
}

/// Page text a long read carries per page: three pages within the story prompt's budget, about.
pub const LONG_READ_PAGE_CHARS: usize = 3_000;

pub const SECTION_SYSTEM: &str = "\
You write one section of a household member's daily news digest, about one of their \
interests, from the numbered sources below: stories the pond wrote today from pages it read, \
and pages it read for this section.

Write only from the sources. Cite with the source's number in square brackets, like [2], at the \
end of the sentence or run of sentences it supports, not after every sentence. Add no facts from \
your own knowledge. Use your own words, and date nothing with today's date. Keep your thinking concise and focused on synthesizing the news for this section.

Reply with JSON and nothing else:
{\"heading\": \"...\", \"body\": \"...\"}

- heading: under 70 characters, naming this section's main news specifically, not just the \
interest's name.
- body: Markdown, 80 to 200 words, in one to three short paragraphs, the most important news \
first. Connect the sources where they connect, and cite each source you use at least once.";

/// One numbered source a section is written from.
pub struct SectionSource<'a> {
    pub site: &'a str,
    pub title: &'a str,
    pub text: &'a str,
}

/// The section prompt's user message.
pub fn section_prompt(
    member: &MemberContext,
    interest: &InterestRow,
    today: &str,
    sources: &[SectionSource<'_>],
) -> String {
    let mut text = format!(
        "{}\n\n{}\n\nToday is {today}.\n\nSources:",
        member.about(),
        interest_line(interest)
    );
    for (i, source) in sources.iter().enumerate() {
        text.push_str(&format!(
            "\n\n[{}] {} - {}\n{}",
            i + 1,
            source.site,
            source.title,
            source.text.trim()
        ));
    }
    text
}

/// A digest section as the model wrote it, its citations already links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSection {
    pub heading: String,
    pub body: String,
}

/// The section in `reply`, each `[n]` turned into a link to source `n`; a number with no source
/// is dropped. `None` when the answer has no usable section.
pub fn parse_section(reply: &str, source_urls: &[String]) -> Option<WrittenSection> {
    let object = json_object(reply)?;
    let heading = one_line(object.get("heading")?.as_str()?);
    let heading = heading.trim_end_matches('.').to_string();
    let body = object.get("body")?.as_str()?.trim().to_string();
    if heading.is_empty() || heading.chars().count() > MAX_HEADING_CHARS {
        return None;
    }
    if body.split_whitespace().count() < MIN_BODY_WORDS {
        return None;
    }
    Some(WrittenSection {
        heading,
        body: one_citation_per_run(&link_citations(&body, source_urls)),
    })
}

/// `[2]`, `[1, 3]` and `[1][3]` become `[2](url)` links; a number with no source goes.
pub fn link_citations(body: &str, urls: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        let already_link = after[close + 1..].starts_with('(');
        let numbers: Option<Vec<usize>> = inner
            .split(',')
            .map(|n| n.trim().parse::<usize>().ok())
            .collect();
        match numbers {
            Some(numbers) if !already_link && !inner.trim().is_empty() => {
                let links: Vec<String> = numbers
                    .into_iter()
                    .filter_map(|n| {
                        let url = urls.get(n.checked_sub(1)?)?;
                        Some(format!("[{n}]({url})"))
                    })
                    .collect();
                if links.is_empty() {
                    let trimmed = out.trim_end().len();
                    out.truncate(trimmed);
                } else {
                    out.push_str(&links.join(""));
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Within each paragraph, a citation repeated by the next one is dropped, so a run of sentences
/// from one source ends with a single citation.
pub fn one_citation_per_run(body: &str) -> String {
    body.split("\n\n")
        .map(|paragraph| {
            let cites: Vec<(usize, usize, &str)> = citation_spans(paragraph);
            let mut out = String::with_capacity(paragraph.len());
            let mut at = 0;
            for (i, (start, end, number)) in cites.iter().enumerate() {
                let repeated = cites.get(i + 1).is_some_and(|(_, _, next)| next == number);
                if repeated {
                    out.push_str(paragraph[at..*start].trim_end_matches(' '));
                    at = *end;
                }
            }
            out.push_str(&paragraph[at..]);
            out
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Each `[n](url)` in `text`: where it starts and ends, and its number.
fn citation_spans(text: &str) -> Vec<(usize, usize, &str)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find('[').map(|i| i + from) {
        let rest = &text[open + 1..];
        let Some(close) = rest.find("](") else {
            break;
        };
        let number = &rest[..close];
        let after = &rest[close + 2..];
        match after.find(')') {
            Some(end) if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) => {
                let stop = open + 1 + close + 2 + end + 1;
                spans.push((open, stop, number));
                from = stop;
            }
            _ => from = open + 1,
        }
    }
    spans
}

pub const INTRO_SYSTEM: &str = "\
You open a household member's daily news digest. Below are its sections: each one's heading \
and first lines.

Reply with JSON and nothing else:
{\"title\": \"...\", \"intro\": \"...\"}

- title: under 60 characters, naming the one or two biggest items. No date, and not the word \
\"digest\".
- intro: exactly two sentences, under 60 words together, telling the member plainly what is in \
today's news. Use only what the sections say; address them as \"you\".";

/// The intro prompt's user message.
pub fn intro_prompt(member: &MemberContext, today: &str, sections: &[(String, String)]) -> String {
    let mut text = format!("{}\n\nToday is {today}.\n\nSections:", member.about());
    for (heading, body) in sections {
        text.push_str(&format!("\n\n## {heading}\n{}", first_sentences(body, 2)));
    }
    text
}

/// The digest's title and two-sentence opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenIntro {
    pub title: String,
    pub intro: String,
}

pub fn parse_intro(reply: &str) -> Option<WrittenIntro> {
    let object = json_object(reply)?;
    let title = one_line(object.get("title")?.as_str()?);
    let intro = one_line(object.get("intro")?.as_str()?);
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS || intro.is_empty() {
        return None;
    }
    Some(WrittenIntro {
        title: title.trim_end_matches('.').to_string(),
        intro: first_sentences(&intro, 2),
    })
}

/// The opening when the model gave none: the sections, named.
pub fn fallback_intro(name: &str, weekday: &str, headings: &[String]) -> WrittenIntro {
    let listed = match headings {
        [] => "nothing new yet".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    WrittenIntro {
        title: format!("{name}'s news for {weekday}"),
        intro: format!(
            "Here is your news for {weekday}: {listed}. Each section links to the pages Goose read."
        ),
    }
}

/// The first JSON object in a model's answer, past any reasoning and code fence.
pub fn json_object(reply: &str) -> Option<serde_json::Map<String, Value>> {
    let text = strip_reasoning(reply);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    match serde_json::from_str::<Value>(&text[start..=end]).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches('"')
        .trim()
        .to_string()
}

/// At most `n` sentences of `text`.
pub fn first_sentences(text: &str, n: usize) -> String {
    let text = one_line(text);
    let mut out = String::new();
    let mut count = 0;
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        out.push(*c);
        let ends = matches!(c, '.' | '!' | '?')
            && chars.get(i + 1).is_none_or(|next| next.is_whitespace());
        if ends {
            count += 1;
            if count == n {
                break;
            }
        }
    }
    out.trim().to_string()
}

/// The page's text and the day, with digit groups' separators removed, for number checks.
fn number_haystack(lines: &[String], today: &str) -> String {
    let mut text = lines.join(" ");
    text.push(' ');
    text.push_str(today);
    squeeze_numbers(&text)
}

fn squeeze_numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, c) in chars.iter().enumerate() {
        let between_digits = i > 0
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
            && chars[i - 1].is_ascii_digit();
        if (*c == ',' || *c == ' ') && between_digits {
            continue;
        }
        out.push(*c);
    }
    out
}

/// Numbers of two or more digits in `text` that `haystack` does not carry.
fn unknown_numbers(text: &str, haystack: &str) -> Vec<String> {
    let squeezed = squeeze_numbers(text);
    let mut unknown = Vec::new();
    let mut current = String::new();
    for c in squeezed.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            current.push(c);
            continue;
        }
        if current.len() >= 2 && !contains_number(haystack, &current) && !unknown.contains(&current)
        {
            unknown.push(current.clone());
        }
        current.clear();
    }
    unknown
}

/// Whether `number` appears in `haystack` as a whole run of digits.
fn contains_number(haystack: &str, number: &str) -> bool {
    haystack.match_indices(number).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + number.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::news::NewsUse;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn summary_of(words: usize) -> String {
        vec!["word"; words].join(" ")
    }

    fn reply(headline: &str, summary: &str) -> String {
        serde_json::json!({
            "headline": headline,
            "dek": "The movement held its first rally at Kamukunji grounds.",
            "summary": summary,
            "why_you": "You follow Kenyan civic politics closely."
        })
        .to_string()
    }

    #[test]
    fn a_story_comes_out_of_fences_and_reasoning() {
        let page = lines("Sifuna said 3,000 people came on 8 November.");
        let fenced = format!(
            "<think>let me see</think>```json\n{}\n```",
            reply(
                "Sifuna draws 3,000 to Linda Mwananchi rally",
                &summary_of(130)
            )
        );
        let story = parse_story(&fenced, &page, "Monday 9 November 2026").unwrap();
        assert_eq!(
            story.headline,
            "Sifuna draws 3,000 to Linda Mwananchi rally"
        );
        assert!(story.why_you.starts_with("You follow"));
    }

    #[test]
    fn a_number_the_page_does_not_carry_refuses_the_story() {
        let page = lines("Sifuna said 3,000 people came.");
        let made_up = reply("Sifuna draws 5,000 to rally", &summary_of(130));
        assert_eq!(
            parse_story(&made_up, &page, "9 November 2026"),
            Err(StoryRefusal::NotOnThePage("5000".into()))
        );
    }

    #[test]
    fn a_skip_or_a_short_summary_gives_no_story() {
        let page = lines("A home page.");
        assert_eq!(
            parse_story(r#"{"skip": "a home page"}"#, &page, "today"),
            Err(StoryRefusal::Skipped("a home page".into()))
        );
        assert_eq!(
            parse_story(
                &reply("Something happened", &summary_of(20)),
                &page,
                "today"
            ),
            Err(StoryRefusal::OutOfBounds("summary"))
        );
        assert_eq!(
            parse_story("I could not do that.", &page, "today"),
            Err(StoryRefusal::Unparseable)
        );
    }

    #[test]
    fn citations_become_links_and_a_missing_source_goes() {
        let urls = vec![
            "https://a.example/1".to_string(),
            "https://b.example/2".to_string(),
        ];
        assert_eq!(
            link_citations(
                "Prices fell [1]. Imports rose [1, 2]. Odd [7]. Kept [x].",
                &urls
            ),
            "Prices fell [1](https://a.example/1). Imports rose \
             [1](https://a.example/1)[2](https://b.example/2). Odd. Kept [x]."
        );
        assert_eq!(
            link_citations("Already [1](https://a.example/1).", &urls),
            "Already [1](https://a.example/1)."
        );
    }

    #[test]
    fn a_run_of_one_source_ends_with_one_citation() {
        let body = "Prices fell [1](https://a/1). Imports rose [1](https://a/1). Duty went up \
                    [2](https://b/2).\n\nNew rules [2](https://b/2). More [2](https://b/2).";
        assert_eq!(
            one_citation_per_run(body),
            "Prices fell. Imports rose [1](https://a/1). Duty went up [2](https://b/2).\n\nNew \
             rules. More [2](https://b/2)."
        );
    }

    #[test]
    fn a_section_needs_a_heading_and_a_body() {
        let urls = vec!["https://a.example/1".to_string()];
        let body = format!("{} [1].", summary_of(40));
        let ok = serde_json::json!({"heading": "Fielders slip under a million", "body": body});
        let section = parse_section(&ok.to_string(), &urls).unwrap();
        assert!(section.body.ends_with("[1](https://a.example/1)."));
        let thin = serde_json::json!({"heading": "Cars", "body": "Prices fell [1]."});
        assert_eq!(parse_section(&thin.to_string(), &urls), None);
    }

    #[test]
    fn the_intro_keeps_two_sentences() {
        let reply =
            r#"{"title": "Sifuna's rally and cheaper Fielders.", "intro": "One. Two. Three."}"#;
        let intro = parse_intro(reply).unwrap();
        assert_eq!(intro.title, "Sifuna's rally and cheaper Fielders");
        assert_eq!(intro.intro, "One. Two.");
        let fallback = fallback_intro("Jerry", "Monday", &["A".into(), "B".into(), "C".into()]);
        assert_eq!(
            fallback.intro,
            "Here is your news for Monday: A, B and C. Each section links to the pages Goose read."
        );
    }

    #[test]
    fn the_member_is_named_and_curiosity_is_not_an_interest() {
        let member = MemberContext {
            name: "Jerry".into(),
            portrait: Some(Portrait {
                summary: "Runs Jarida from Nairobi.".into(),
                into: vec!["MCP".into()],
                ..Default::default()
            }),
            interests: vec!["MCP".into()],
        };
        let about = member.about();
        assert!(about.starts_with("You are writing for Jerry."));
        assert!(about.contains("Runs Jarida from Nairobi."));
        assert!(about.contains("curiosity, not an interest"));
        let interest = InterestRow {
            id: "i".into(),
            topic: "The Boys".into(),
            category: "film_tv".into(),
            facets: vec!["Gen V".into()],
            depth: "keen".into(),
            news: NewsUse::Both,
            avoid: false,
            evidence: 3,
            confirmed: true,
        };
        assert_eq!(
            interest_line(&interest),
            "The interest: The Boys (what they follow in it: Gen V). They are keen about it."
        );
    }

    #[test]
    fn page_text_stops_at_its_budget_on_a_line() {
        let page = lines("first line\nsecond line\nthird line");
        assert_eq!(page_text(&page, 23), "first line\nsecond line");
    }
}
