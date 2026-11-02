//! A long task's report as a file a person can keep: Markdown as it is, or a self-contained HTML
//! page that prints well and that a browser or a phone can save as a PDF.
//!
//! The Markdown was written by a model from web pages, so nothing in it reaches the page as
//! markup: raw HTML is shown as text, only web and mail links become links, and images are never
//! loaded, since a saved report must not call out to a server when it is opened.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// The report's body as HTML, from its Markdown.
pub fn markdown_to_html(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut out = String::with_capacity(markdown.len() * 2);
    // Whether each open link was safe to render as one, so its end tag matches.
    let mut links: Vec<bool> = Vec::new();
    let mut in_table_head = false;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.push_str("<p>"),
                Tag::Heading { level, .. } => out.push_str(&format!("<{level}>")),
                Tag::BlockQuote(_) => out.push_str("<blockquote>"),
                Tag::CodeBlock(_) => out.push_str("<pre><code>"),
                Tag::List(Some(start)) if start != 1 => {
                    out.push_str(&format!("<ol start=\"{start}\">"))
                }
                Tag::List(Some(_)) => out.push_str("<ol>"),
                Tag::List(None) => out.push_str("<ul>"),
                Tag::Item => out.push_str("<li>"),
                Tag::Table(_) => out.push_str("<table>"),
                Tag::TableHead => {
                    in_table_head = true;
                    out.push_str("<thead><tr>");
                }
                Tag::TableRow => out.push_str("<tr>"),
                Tag::TableCell => out.push_str(if in_table_head { "<th>" } else { "<td>" }),
                Tag::Emphasis => out.push_str("<em>"),
                Tag::Strong => out.push_str("<strong>"),
                Tag::Strikethrough => out.push_str("<del>"),
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                    let safe = is_safe_link(&dest_url);
                    if safe {
                        out.push_str(&format!("<a href=\"{}\">", escape(&dest_url)));
                    }
                    links.push(safe);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out.push_str("</p>\n"),
                TagEnd::Heading(level) => out.push_str(&format!("</{level}>\n")),
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
                TagEnd::CodeBlock => out.push_str("</code></pre>\n"),
                TagEnd::List(true) => out.push_str("</ol>\n"),
                TagEnd::List(false) => out.push_str("</ul>\n"),
                TagEnd::Item => out.push_str("</li>\n"),
                TagEnd::Table => out.push_str("</tbody></table>\n"),
                TagEnd::TableHead => {
                    in_table_head = false;
                    out.push_str("</tr></thead><tbody>\n");
                }
                TagEnd::TableRow => out.push_str("</tr>\n"),
                TagEnd::TableCell => out.push_str(if in_table_head { "</th>" } else { "</td>" }),
                TagEnd::Emphasis => out.push_str("</em>"),
                TagEnd::Strong => out.push_str("</strong>"),
                TagEnd::Strikethrough => out.push_str("</del>"),
                TagEnd::Link | TagEnd::Image => {
                    if links.pop().unwrap_or(false) {
                        out.push_str("</a>");
                    }
                }
                _ => {}
            },
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push_str(&escape(&text))
            }
            Event::Code(code) => out.push_str(&format!("<code>{}</code>", escape(&code))),
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>\n"),
            Event::Rule => out.push_str("<hr>\n"),
            Event::TaskListMarker(done) => out.push_str(if done {
                "<input type=\"checkbox\" checked disabled> "
            } else {
                "<input type=\"checkbox\" disabled> "
            }),
            _ => {}
        }
    }
    out
}

/// The whole page: one file with its own styles, nothing fetched.
pub fn html_document(title: &str, markdown: &str, made_on: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<main>\n{body}</main>\n\
         <footer>Made by Goose In A Pond on {made_on}.</footer>\n</body>\n</html>\n",
        title = escape(title),
        body = markdown_to_html(markdown),
        made_on = escape(made_on),
    )
}

const STYLE: &str = "\
:root{color-scheme:light;}\
body{font:16px/1.6 -apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1a1a1a;\
background:#fff;margin:0;padding:24px 16px;}\
main{max-width:760px;margin:0 auto;}\
h1{font-size:1.9em;line-height:1.25;margin:0 0 .6em;}\
h2{font-size:1.35em;margin:1.6em 0 .5em;border-bottom:1px solid #ddd;padding-bottom:.2em;}\
h3{font-size:1.1em;margin:1.3em 0 .4em;}\
table{border-collapse:collapse;width:100%;margin:1em 0;font-size:.95em;}\
th,td{border:1px solid #ccc;padding:6px 8px;text-align:left;vertical-align:top;}\
th{background:#f3f3f3;}\
li{margin:.2em 0;}\
li input[type=checkbox]{margin-right:.4em;}\
pre{background:#f6f6f6;padding:12px;overflow-x:auto;}\
code{font-family:ui-monospace,Menlo,Consolas,monospace;font-size:.92em;}\
blockquote{margin:1em 0;padding-left:1em;border-left:3px solid #ccc;color:#444;}\
a{color:#0b57d0;}\
footer{max-width:760px;margin:2.5em auto 0;color:#666;font-size:.85em;}\
@media print{body{padding:0;}a{color:inherit;}h2{break-after:avoid;}table,pre{break-inside:avoid;}}";

fn is_safe_link(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:")
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A file name from the report's title: lowercase words joined by dashes, never empty.
pub fn file_stem(title: &str) -> String {
    let mut stem = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if stem.len() + word.len() + 1 > 60 {
            break;
        }
        if !stem.is_empty() {
            stem.push('-');
        }
        stem.push_str(&word.to_ascii_lowercase());
    }
    if stem.is_empty() {
        "report".to_string()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_in_the_report_is_shown_never_run() {
        let html = markdown_to_html(
            "Hi <script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n\
             [ok](https://example.org) [bad](javascript:alert(1)) ![map](https://tiles.example/x.png)",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("<a href=\"https://example.org\">ok</a>"));
        assert!(
            !html.contains("javascript:"),
            "an unsafe link keeps only its text: {html}"
        );
        assert!(
            html.contains("<a href=\"https://tiles.example/x.png\">map</a>"),
            "an image is a link to it, never fetched: {html}"
        );
    }

    #[test]
    fn tables_and_checklists_survive_the_export() {
        let html = markdown_to_html(
            "| Day | Lunch |\n|---|---|\n| Fri | Biryani |\n\n- [ ] Sunscreen\n- [x] Tickets\n",
        );
        assert!(html.contains("<thead><tr><th>Day</th><th>Lunch</th></tr></thead><tbody>"));
        assert!(html.contains("<tr><td>Fri</td><td>Biryani</td></tr>"));
        assert!(html.contains("<input type=\"checkbox\" disabled> Sunscreen"));
        assert!(html.contains("<input type=\"checkbox\" checked disabled> Tickets"));
    }

    #[test]
    fn the_page_stands_alone_and_its_title_is_escaped() {
        let page = html_document("Fish & chips <b>", "# Fish", "29 October 2026");
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>Fish &amp; chips &lt;b&gt;</title>"));
        assert!(
            !page.contains("<link") && !page.contains("<script"),
            "nothing fetched or run"
        );
    }

    #[test]
    fn a_file_name_comes_from_the_title() {
        assert_eq!(
            file_stem("Our Mombasa trip: 4 days!"),
            "our-mombasa-trip-4-days"
        );
        assert_eq!(file_stem("***"), "report");
        assert!(file_stem(&"longword ".repeat(20)).len() <= 60);
    }
}
