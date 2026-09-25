//! Minimal WebDAV `multistatus` reader. Matches LOCAL names only, since servers disagree on
//! namespace prefixes (`d:href`, `D:href`, `href`).

use quick_xml::events::Event;
use quick_xml::Reader;

/// One `<response>` element, reduced to the parts a connector needs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DavResponse {
    pub href: String,
    /// `<resourcetype>` children by local name, e.g. `collection`, `calendar`.
    pub resource_types: Vec<String>,
    /// Text of `<displayname>`, when the server sent one.
    pub display_name: Option<String>,
    /// Text of `<calendar-data>`, i.e. the iCalendar document itself.
    pub calendar_data: Option<String>,
    /// `<getctag>` or `<sync-token>`: what makes the next sync incremental.
    pub ctag: Option<String>,
    /// Href inside `<current-user-principal>`/`<calendar-home-set>`: discovery's next URL.
    pub nested_href: Option<String>,
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or("").to_ascii_lowercase()
}

/// Parse a `multistatus` into its responses; unknown elements are skipped, not refused.
pub fn parse_multistatus(xml: &str) -> anyhow::Result<Vec<DavResponse>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut responses = Vec::new();
    let mut current: Option<DavResponse> = None;
    // Local-name element stack, so text is attributed to its enclosing property.
    let mut stack: Vec<String> = Vec::new();
    let mut seen_response_href = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref());
                if name == "response" {
                    current = Some(DavResponse::default());
                    seen_response_href = false;
                }
                if name == "resourcetype" {
                    // Its children are self-closing; the `Empty` arm collects them.
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = local(e.name().as_ref());
                if stack.last().map(String::as_str) == Some("resourcetype") {
                    if let Some(cur) = current.as_mut() {
                        cur.resource_types.push(name);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let Some(cur) = current.as_mut() else {
                    continue;
                };
                match stack.last().map(String::as_str) {
                    Some("href") => {
                        // First href is the response's; one nested in a property is the next hop.
                        if !seen_response_href
                            && !stack.iter().rev().skip(1).any(|s| {
                                s == "current-user-principal"
                                    || s == "calendar-home-set"
                                    || s == "owner"
                            })
                        {
                            cur.href = text;
                            seen_response_href = true;
                        } else {
                            cur.nested_href.get_or_insert(text);
                        }
                    }
                    Some("displayname") => cur.display_name = Some(text),
                    Some("calendar-data") => cur.calendar_data = Some(text),
                    Some("getctag") | Some("sync-token") => cur.ctag = Some(text),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = local(e.name().as_ref());
                stack.pop();
                if name == "response" {
                    if let Some(cur) = current.take() {
                        responses.push(cur);
                    }
                }
            }
            Ok(Event::Eof) => {
                // quick-xml reports truncation as plain Eof; refuse it rather than look empty.
                if !stack.is_empty() || current.is_some() {
                    return Err(anyhow::anyhow!(
                        "the calendar server's reply ended early, with {} element(s) unclosed",
                        stack.len()
                    ));
                }
                break;
            }
            Err(e) => return Err(anyhow::anyhow!("malformed CalDAV response: {e}")),
            _ => {}
        }
    }
    Ok(responses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_calendar_collection_is_recognised_whatever_prefix_the_server_uses() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/calendars/jerry/home/</D:href>
    <D:propstat><D:prop>
      <D:displayname>Home</D:displayname>
      <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
      <CS:getctag xmlns:CS="http://calendarserver.org/ns/">tag-1</CS:getctag>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].href, "/calendars/jerry/home/");
        assert_eq!(r[0].display_name.as_deref(), Some("Home"));
        assert!(r[0].resource_types.iter().any(|t| t == "calendar"));
        assert_eq!(r[0].ctag.as_deref(), Some("tag-1"));
    }

    #[test]
    fn an_unprefixed_document_parses_identically() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/c/</href>
    <propstat><prop><resourcetype><collection/><calendar/></resourcetype></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].href, "/c/");
        assert!(r[0].resource_types.iter().any(|t| t == "calendar"));
    }

    #[test]
    fn an_href_inside_a_property_is_kept_apart_from_the_responses_own() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/</href>
    <propstat><prop>
      <current-user-principal><href>/principals/jerry/</href></current-user-principal>
    </prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].href, "/");
        assert_eq!(r[0].nested_href.as_deref(), Some("/principals/jerry/"));
    }

    #[test]
    fn calendar_data_comes_back_whole() {
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response><href>/c/e1.ics</href>
    <propstat><prop><C:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</C:calendar-data></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert!(r[0].calendar_data.as_deref().unwrap().contains("VCALENDAR"));
    }

    #[test]
    fn unknown_properties_are_skipped_rather_than_refused() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/c/</href>
    <propstat><prop><some-vendor-extension>x</some-vendor-extension>
      <displayname>Work</displayname></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].display_name.as_deref(), Some("Work"));
    }

    #[test]
    fn a_truncated_reply_is_an_error_not_an_empty_list() {
        let err = parse_multistatus("<multistatus><response><href>/c/</href>").unwrap_err();
        assert!(err.to_string().contains("ended early"), "{err}");
        // The guard must not refuse a well-formed empty reply.
        assert!(
            parse_multistatus("<multistatus xmlns=\"DAV:\"></multistatus>")
                .unwrap()
                .is_empty()
        );
    }
}
