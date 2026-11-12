//! POST forms for `read_web_page`. Asked for like a GET form, a POST form is not sent: the pond
//! keeps it and puts it on the speaker's own phone, and sends it only on their yes
//! (`web_form_confirm`). Asked for again, the conversation hears how it went, and a sent form's
//! answer reads as a page one click past the form's.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use pond_core::mcp::ports::notification::{MemberDelivery, Notification};
use pond_core::mcp::services::page_addresses::{FormField, FormMethod, PostRequest, SearchForm};
use pond_core::shared::domain::web_form::{FormSubmission, SubmissionState, ANSWER_WITHIN_MINUTES};
use pond_core::shared::ports::web_form::{FormSender, FormSubmissionStore, SendFailure, SentPage};
use pond_core::shared::services::egress::EgressPurpose;
use pond_core::shared::services::passages;
use pond_core::shared::services::web_form_confirm::{self, FormConfirmations, FormRequest};
use pond_core::user_data::domain::profile::ProfileScope;
use rmcp::model::RequestMetaObject;

use crate::format::format_dead_end;
use crate::web_page::{forms_on, keep_answer, ACCEPT, PAGE_TIMEOUT};
use crate::web_search;

const WHAT: &str = "sending that form";

/// The fields a phone notice lists at most, in bytes.
const NOTICE_FIELDS_BYTES: usize = 220;

/// Installs POST forms for this process: kept in `store`, sent through the public-only client.
/// Call once at startup in each binary that runs tools or serves the phone's routes.
pub fn install(store: Arc<dyn FormSubmissionStore>) {
    web_form_confirm::install(Arc::new(FormConfirmations::new(
        store,
        Arc::new(PostFormSender::new()),
    )));
}

/// What `read_web_page` does with a POST form's address.
pub enum FormAnswer {
    /// Say this and read nothing.
    Said(String),
    /// The form was sent: read its answer, after `note`.
    Read {
        address: String,
        depth: u8,
        note: String,
    },
}

/// Forms whose end the conversation has heard; asked for again, such a form is asked anew.
static TOLD: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn tell_once(id: &str) -> bool {
    let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
    told.get_or_insert_with(HashSet::new).insert(id.to_string())
}

/// A POST form's address, asked for in a turn: how it went, or a new ask on the speaker's phone.
pub async fn ask_to_send(meta: &RequestMetaObject, request: PostRequest) -> FormAnswer {
    let Some(forms) = web_form_confirm::installed() else {
        return FormAnswer::Said(format_dead_end(
            WHAT,
            "Sending forms is not set up on this pond.",
        ));
    };
    let owner = match speaking_member(meta).await {
        Ok(id) => id,
        Err(why) => {
            return FormAnswer::Said(format_dead_end(
                WHAT,
                &format!(
                    "This form sends with POST, which needs a yes on a household member's own \
                     phone, and {why}."
                ),
            ))
        }
    };
    let now = Utc::now();
    match forms
        .latest(&owner, &request.action, &request.values, now)
        .await
    {
        Ok(Some(earlier)) => {
            if let Some(answer) = earlier_answer(&earlier) {
                return answer;
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "web_form_read_failed", error = %e);
            return FormAnswer::Said(format_dead_end(
                WHAT,
                "The pond couldn't check this form's earlier answer just now; try again shortly.",
            ));
        }
    }
    let Some(notifier) = crate::member_notifier() else {
        return FormAnswer::Said(format_dead_end(
            WHAT,
            "This pond cannot reach phones, so the form was not sent.",
        ));
    };
    let submission = match forms
        .ask(
            FormRequest {
                owner: owner.clone(),
                page: request.page,
                action: request.action,
                values: request.values,
                depth: request.depth,
                session_id: crate::session_from_meta(meta),
            },
            now,
        )
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "web_form_store_failed", error = %e);
            return FormAnswer::Said(format_dead_end(
                WHAT,
                "The pond couldn't keep the form to confirm just now; try again shortly.",
            ));
        }
    };
    let site = submission.site();
    let delivery = notifier
        .notify_member(&owner, confirm_notice(&submission))
        .await;
    let unsent = match delivery {
        MemberDelivery::Reached(_) => None,
        MemberDelivery::NoPhone => Some("the speaker has no paired phone of their own"),
        MemberDelivery::Failed(why) => {
            tracing::warn!(target: "giap::trace", kind = "web_form_notice_failed", error = %why);
            Some("putting it on the speaker's phone failed on the pond")
        }
    };
    tracing::info!(
        target: "giap::trace",
        kind = "web_form_asked",
        host = %site,
        fields = submission.values.len(),
        reached = unsent.is_none(),
    );
    if let Some(why) = unsent {
        let _ = forms
            .abandon(&submission.id, &format!("Not sent: {why}."), Utc::now())
            .await;
        tell_once(&submission.id);
        return FormAnswer::Said(format_dead_end(WHAT, &format!("Not sent: {why}.")));
    }
    FormAnswer::Said(format!(
        "Nothing has been sent to {site} yet. This form sends with POST, so it waits for the \
         speaker's yes on their own phone, where it now shows with its fields: {}. It lapses \
         unsent if they don't answer within {ANSWER_WITHIN_MINUTES} minutes. Once they answer, \
         reading this same address again gives the outcome.",
        fields_words(&submission.values)
    ))
}

/// How an earlier ask of the same form went, or `None` to ask again.
fn earlier_answer(earlier: &FormSubmission) -> Option<FormAnswer> {
    let site = earlier.site();
    let said = |text: String| Some(FormAnswer::Said(text));
    match &earlier.state {
        SubmissionState::Waiting => said(format!(
            "Nothing has been sent to {site} yet: the form is on the speaker's phone, waiting for \
             their yes. It lapses unsent at {} UTC.",
            earlier.answer_by().format("%H:%M")
        )),
        SubmissionState::Sending => said(format!(
            "The speaker said yes on their phone, and the form is being sent to {site} now."
        )),
        _ if !tell_once(&earlier.id) => None,
        SubmissionState::Sent {
            status, address, ..
        } if (200..400).contains(status) => Some(FormAnswer::Read {
            address: address.clone(),
            depth: earlier.depth.saturating_add(1),
            note: format!(
                "The speaker said yes on their phone and the form was sent to {site}. The \
                     site's answer:"
            ),
        }),
        SubmissionState::Sent { status, .. } => said(format!(
            "The speaker said yes on their phone and the form was sent to {site}, which answered \
             HTTP {status}: it did not accept it."
        )),
        SubmissionState::Declined => said(format!(
            "The speaker said no on their phone, so nothing was sent to {site}."
        )),
        SubmissionState::TimedOut => said(format!(
            "No answer came from the speaker's phone within {ANSWER_WITHIN_MINUTES} minutes, so \
             nothing was sent to {site}."
        )),
        SubmissionState::Failed { why } => said(format!("The form to {site} was not sent: {why}")),
    }
}

/// The confirm a member's phone shows: where it goes and what it says. The phone reads the
/// rest from `GET /api/v1/web-forms/{id}`.
fn confirm_notice(submission: &FormSubmission) -> Notification {
    let site = submission.site();
    Notification {
        id: uuid::Uuid::new_v4().to_string(),
        target: submission.owner.clone(),
        category: "action_required".to_string(),
        title: format!("Send this form to {site}?"),
        body: format!(
            "{}. Nothing is sent until you confirm.",
            passages::clip(&fields_words(&submission.values), NOTICE_FIELDS_BYTES)
        ),
        timestamp: Utc::now().to_rfc3339(),
        data: Some(serde_json::json!({
            "action": "form_confirm",
            "id": submission.id,
            "site": site,
            "label": "Review",
        })),
    }
}

fn fields_words(values: &[(String, String)]) -> String {
    values
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The household member speaking in this call, or why there is none. An unidentified speaker in
/// a one-member household can only be that member; a guest is nobody's.
async fn speaking_member(meta: &RequestMetaObject) -> Result<String, &'static str> {
    const NOT_ONE_MEMBER: &str = "the speaker is not identified as one household member";
    let authority = crate::speaker_authority().ok_or("this pond cannot tell who is speaking")?;
    let session = crate::session_from_meta(meta).ok_or("this call carries no session")?;
    match authority.actor_for_engine_session(&session).await {
        Some((ProfileScope::Owner(id), _)) => Ok(id),
        Some((ProfileScope::Household, _)) => authority.sole_member().await.ok_or(NOT_ONE_MEMBER),
        _ => Err(NOT_ONE_MEMBER),
    }
}

// ── Sending ────────────────────────────────────────────────────────────────

/// Sends a confirmed form the way a browser would: the page it sits on is read again for the
/// form's fresh hidden values and the site's cookies, then the form is posted with them.
pub struct PostFormSender {
    client: Option<reqwest::Client>,
}

impl PostFormSender {
    /// Sends through the page reader's public-only client.
    pub fn new() -> Self {
        Self { client: None }
    }

    #[cfg(test)]
    fn with_client(client: reqwest::Client) -> Self {
        Self {
            client: Some(client),
        }
    }

    fn client(&self) -> Option<&reqwest::Client> {
        self.client.as_ref().or_else(|| web_search::page_client())
    }
}

impl Default for PostFormSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FormSender for PostFormSender {
    async fn send(&self, submission: &FormSubmission) -> Result<SentPage, SendFailure> {
        let not_sent = |why: &str| SendFailure::NotSent(why.to_string());
        let client = self
            .client()
            .ok_or_else(|| not_sent("The pond's page client could not be built."))?;
        for address in [&submission.page, &submission.action] {
            let public = reqwest::Url::parse(address)
                .ok()
                .is_some_and(|u| pond_net::public_only::check_target(&u).is_ok());
            if !public {
                return Err(not_sent("The form's site is not on the public web."));
            }
        }

        // Filed under the conversation that asked, never the turn in flight on this pond.
        let session = submission.session_id.as_deref().unwrap_or_default();
        let page = crate::http::traced_get_for(
            client,
            &submission.page,
            EgressPurpose::WebForms,
            session,
            |b| {
                b.timeout(PAGE_TIMEOUT)
                    .header(reqwest::header::ACCEPT, ACCEPT)
            },
        )
        .await
        .map_err(|e| {
            SendFailure::NotSent(format!(
                "The page with the form could not be read: {}.",
                web_search::describe_failure(&e)
            ))
        })?;
        if !page.status().is_success() {
            return Err(SendFailure::NotSent(format!(
                "The page with the form answered HTTP {}.",
                page.status().as_u16()
            )));
        }
        let page_address = page.url().to_string();
        let cookies = cookies_of(page.headers());
        let html = web_search::read_capped(page, web_search::PAGE_BYTE_CAP)
            .await
            .ok_or_else(|| not_sent("The page with the form stopped arriving."))?;
        let fresh = forms_on(page_address.clone(), html)
            .await
            .into_iter()
            .find(|f| f.method == FormMethod::Post && same_action(&f.action, &submission.action))
            .ok_or_else(|| not_sent("The form is no longer on its page."))?;
        let fields = filled(&fresh, &submission.values).map_err(SendFailure::NotSent)?;

        let answer = crate::http::traced_post_form_for(
            client,
            &fresh.action,
            &fields,
            EgressPurpose::WebForms,
            session,
            |b| {
                let b = b
                    .timeout(PAGE_TIMEOUT)
                    .header(reqwest::header::ACCEPT, ACCEPT)
                    .header(reqwest::header::REFERER, page_address.as_str());
                match &cookies {
                    Some(c) => b.header(reqwest::header::COOKIE, c.as_str()),
                    None => b,
                }
            },
        )
        .await
        .map_err(|e| lost_or_unsent(&e))?;
        let status = answer.status().as_u16();
        let address = answer.url().to_string();
        let html = if answer.status().is_success() {
            web_search::read_capped(answer, web_search::PAGE_BYTE_CAP).await
        } else {
            None
        };
        let title = match html {
            Some(html) => {
                keep_answer(address.clone(), html, submission.depth.saturating_add(1)).await
            }
            None => String::new(),
        };
        tracing::info!(
            target: "giap::trace",
            kind = "web_form_answered",
            host = %web_search::domain_of(&address),
            status,
        );
        Ok(SentPage {
            status,
            address,
            title,
        })
    }
}

/// A refusal before any socket opened sent nothing; a connection that never formed sent nothing;
/// anything after the request went out may have reached the site.
fn lost_or_unsent(error: &anyhow::Error) -> SendFailure {
    let why = web_search::describe_failure(error);
    match error.downcast_ref::<reqwest::Error>() {
        Some(e) if e.is_connect() || e.is_builder() => {
            SendFailure::NotSent(format!("The site could not be reached: {why}."))
        }
        Some(_) => SendFailure::AnswerLost(format!("The site did not answer: {why}.")),
        None => SendFailure::NotSent(format!("The pond would not send to the site: {why}.")),
    }
}

/// The `name=value` pairs of a page's cookies, to send back with its form.
fn cookies_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let pairs: Vec<&str> = headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .map(str::trim)
        .filter(|p| p.contains('=') && !p.starts_with('='))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// Whether two actions are one form's: the same host, `www.` aside, and path.
fn same_action(a: &str, b: &str) -> bool {
    let key = |address: &str| {
        reqwest::Url::parse(address).ok().map(|u| {
            let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
            let host = host
                .strip_prefix("www.")
                .map(str::to_string)
                .unwrap_or(host);
            format!("{host}{}", u.path().trim_end_matches('/'))
        })
    };
    key(a).is_some_and(|k| Some(k) == key(b))
}

/// The fields to post: the fresh form's own hidden values, and the member's confirmed ones, each
/// still one the form takes. `Err` says what changed on the page.
fn filled(
    form: &SearchForm,
    confirmed: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    for (name, value) in confirmed {
        let field = form
            .fields
            .iter()
            .find(|f| f.name() == name)
            .ok_or_else(|| format!("The form no longer has a {name} field."))?;
        let admitted = match field {
            FormField::Fixed { .. } => true,
            FormField::Text(_) => {
                value.chars().count()
                    <= pond_core::mcp::services::page_addresses::MAX_FORM_VALUE_CHARS
            }
            FormField::Choice { options, .. } => options.iter().any(|o| o == value),
        };
        if !admitted {
            return Err(format!("The form no longer takes \"{value}\" for {name}."));
        }
    }
    let mut fields = Vec::new();
    for field in &form.fields {
        match field {
            FormField::Fixed { name, value } => fields.push((name.clone(), value.clone())),
            other => {
                fields.extend(
                    confirmed
                        .iter()
                        .filter(|(name, _)| name == other.name())
                        .cloned(),
                );
            }
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn post_form() -> SearchForm {
        SearchForm {
            action: "https://cars.example/enquire".into(),
            method: FormMethod::Post,
            fields: vec![
                FormField::Fixed {
                    name: "csrf".into(),
                    value: "fresh".into(),
                },
                FormField::Text("message".into()),
                FormField::Choice {
                    name: "contact".into(),
                    options: vec!["whatsapp".into(), "sms".into()],
                },
            ],
        }
    }

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_form_posts_its_fresh_hidden_values_and_the_confirmed_ones() {
        let fields = filled(&post_form(), &pairs(&[("message", "Still for sale?")])).unwrap();
        assert_eq!(
            fields,
            pairs(&[("csrf", "fresh"), ("message", "Still for sale?")])
        );
    }

    #[test]
    fn a_form_whose_page_changed_is_not_sent() {
        let changed = filled(&post_form(), &pairs(&[("phone_ok", "yes")])).unwrap_err();
        assert!(
            changed.contains("no longer has a phone_ok field"),
            "{changed}"
        );
        let gone = filled(&post_form(), &pairs(&[("contact", "email")])).unwrap_err();
        assert!(gone.contains("no longer takes \"email\""), "{gone}");
    }

    #[test]
    fn a_pages_cookies_go_back_as_name_value_pairs() {
        let mut headers = reqwest::header::HeaderMap::new();
        for cookie in ["sid=abc; Path=/; HttpOnly", "pref=1", "=bad"] {
            headers.append(reqwest::header::SET_COOKIE, cookie.parse().unwrap());
        }
        assert_eq!(cookies_of(&headers).as_deref(), Some("sid=abc; pref=1"));
        assert_eq!(cookies_of(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn one_form_is_known_by_host_and_path() {
        assert!(same_action(
            "https://www.cars.example/enquire/",
            "https://cars.example/enquire"
        ));
        assert!(!same_action(
            "https://cars.example/enquire",
            "https://cars.example/contact"
        ));
    }

    fn submission(state: SubmissionState) -> FormSubmission {
        FormSubmission {
            id: uuid::Uuid::new_v4().to_string(),
            owner: "jerry".into(),
            page: "https://cars.example/used/9".into(),
            action: "https://cars.example/enquire".into(),
            values: pairs(&[("message", "Still for sale?")]),
            depth: 1,
            session_id: None,
            state,
            asked_at: Utc::now(),
            decided_at: None,
        }
    }

    fn said(answer: Option<FormAnswer>) -> String {
        match answer {
            Some(FormAnswer::Said(text)) => text,
            Some(FormAnswer::Read { note, .. }) => format!("READ {note}"),
            None => "ASK AGAIN".into(),
        }
    }

    #[test]
    fn an_earlier_end_is_told_once_then_the_form_is_asked_anew() {
        let declined = submission(SubmissionState::Declined);
        assert!(said(earlier_answer(&declined)).contains("said no"));
        assert_eq!(said(earlier_answer(&declined)), "ASK AGAIN");

        let waiting = submission(SubmissionState::Waiting);
        for _ in 0..2 {
            assert!(said(earlier_answer(&waiting)).contains("Nothing has been sent"));
        }

        let lapsed = submission(SubmissionState::TimedOut);
        assert!(said(earlier_answer(&lapsed)).contains("No answer came"));

        let sent = submission(SubmissionState::Sent {
            status: 200,
            address: "https://cars.example/thanks".into(),
            title: "Thanks".into(),
        });
        match earlier_answer(&sent) {
            Some(FormAnswer::Read { address, depth, .. }) => {
                assert_eq!(address, "https://cars.example/thanks");
                assert_eq!(depth, 2, "one click past the page the form sits on");
            }
            _ => panic!("a sent form's answer is read"),
        }

        let refused = submission(SubmissionState::Sent {
            status: 403,
            address: "https://cars.example/enquire".into(),
            title: String::new(),
        });
        assert!(said(earlier_answer(&refused)).contains("HTTP 403"));
    }

    #[test]
    fn the_phone_notice_names_the_site_and_what_is_sent() {
        let notice = confirm_notice(&submission(SubmissionState::Waiting));
        assert_eq!(notice.category, "action_required");
        assert_eq!(notice.title, "Send this form to cars.example?");
        assert!(
            notice.body.starts_with("message: Still for sale?."),
            "{}",
            notice.body
        );
        let data = notice.data.unwrap();
        assert_eq!(data["action"], "form_confirm");
        assert_eq!(data["site"], "cars.example");
    }

    /// One request as the stub saw it: its request line, its cookie and its body.
    struct Seen {
        line: String,
        cookie: Option<String>,
        body: String,
    }

    /// Answers a GET of `/used/9` with a POST form and a cookie, and a POST to `/enquire` with a
    /// thank-you page, recording each request.
    fn site() -> (u16, mpsc::Receiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut cookie) = (0, None);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "cookie" => cookie = Some(value.trim().to_string()),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let page = if line.starts_with("GET") {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: sid=s3ss; Path=/\r\n"
                        .to_string()
                        + "Connection: close\r\n\r\n<html><body><form method=post action=/enquire>\
                           <input type=hidden name=csrf value=fresh><textarea name=message>\
                           </textarea></form></body></html>"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                     <html><head><title>Message sent</title></head><body><p>The seller will \
                     reply soon.</p></body></html>"
                        .to_string()
                };
                stream.write_all(page.as_bytes()).unwrap();
                tx.send(Seen {
                    line,
                    cookie,
                    body: String::from_utf8(body).unwrap(),
                })
                .unwrap();
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn a_confirmed_form_is_posted_with_the_pages_cookie_and_its_answer_kept() {
        let (port, seen) = site();
        let client = pond_net::public_only::client_builder(3)
            .resolve(
                "page.test",
                std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            )
            .build()
            .unwrap();
        let sender = PostFormSender::with_client(client);
        let mut form = submission(SubmissionState::Sending);
        form.page = format!("http://page.test:{port}/used/9");
        form.action = format!("http://page.test:{port}/enquire");
        form.values = pairs(&[("message", "Still for sale?"), ("csrf", "stale")]);
        let sent = sender.send(&form).await.expect("sent");
        assert_eq!(sent.status, 200);
        assert_eq!(sent.title, "Message sent");

        let get = seen.recv().unwrap();
        assert!(get.line.starts_with("GET /used/9"), "{}", get.line);
        let post = seen.recv().unwrap();
        assert!(post.line.starts_with("POST /enquire"), "{}", post.line);
        assert_eq!(post.cookie.as_deref(), Some("sid=s3ss"));
        assert_eq!(post.body, "csrf=fresh&message=Still+for+sale%3F");

        let answer = pond_core::mcp::services::page_addresses::global()
            .resolve_with_depth(&sent.address)
            .expect("the answer is readable");
        assert_eq!(answer.1, 2);
    }
}
