//! Web search as the pond offers it: the backend's address and the states the settings screens
//! report. The key lives in the secret store under [`super::tool_group::BRAVE_SEARCH_API_KEY`].

use std::time::Duration;

/// The backend's name, as `GET /api/v1/web-search` reports it.
pub const WEB_SEARCH_PROVIDER: &str = "brave";

/// Origin of the Brave Search API.
pub const BRAVE_API_BASE: &str = "https://api.search.brave.com";

/// Brave's web search endpoint, under [`BRAVE_API_BASE`].
pub const BRAVE_WEB_SEARCH_PATH: &str = "/res/v1/web/search";

/// Brave's LLM Context endpoint, under [`BRAVE_API_BASE`]: ranked page text for one query.
pub const BRAVE_LLM_CONTEXT_PATH: &str = "/res/v1/llm/context";

/// How long the LLM Context request may take before search_web falls back to reading pages.
/// Longer than [`BRAVE_TIMEOUT`]: Brave fetches and extracts the pages inside this one call.
pub const BRAVE_LLM_CONTEXT_TIMEOUT: Duration = Duration::from_secs(8);

/// The header naming the household's town, sent only with local places turned on.
pub const BRAVE_LOC_CITY_HEADER: &str = "X-Loc-City";

/// A town as the header carries it, at most.
const MAX_TOWN_BYTES: usize = 100;

/// Request header carrying the subscription key.
pub const BRAVE_TOKEN_HEADER: &str = "X-Subscription-Token";

/// How long one Brave request may take, the key check included.
pub const BRAVE_TIMEOUT: Duration = Duration::from_secs(6);

/// What the settings screens say about web search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchStatus {
    /// A key is stored and nothing has said it is unusable.
    Ok,
    /// No key is stored.
    NotConfigured,
    /// Brave refused the key (401 or 403).
    Rejected,
    /// The key is valid but this month's allowance is spent (402 or 429).
    Quota,
    /// Stored while Brave could not be reached; the first search checks it.
    Unverified,
}

impl WebSearchStatus {
    /// The wire spelling both apps match on.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotConfigured => "not_configured",
            Self::Rejected => "rejected",
            Self::Quota => "quota",
            Self::Unverified => "unverified",
        }
    }

    /// What one Brave answer says about the key; `None` for a status that says nothing about it.
    pub fn from_brave_http(status: u16) -> Option<Self> {
        match status {
            200..=299 => Some(Self::Ok),
            401 | 403 => Some(Self::Rejected),
            402 | 429 => Some(Self::Quota),
            _ => None,
        }
    }
}

/// What search_web does with one LLM Context answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmContextOutcome {
    /// Grounding came back; it is read.
    Answered,
    /// Brave refused the key: the same verdict a web search would give.
    Rejected,
    /// The allowance is spent: the same verdict a web search would give.
    Quota,
    /// Anything else, a key on a plan without LLM Context included: search and read pages.
    Fallback,
}

impl LlmContextOutcome {
    pub fn from_http(status: u16) -> Self {
        match status {
            200..=299 => Self::Answered,
            401 => Self::Rejected,
            429 => Self::Quota,
            _ => Self::Fallback,
        }
    }
}

/// The town in a stored place name: the text before its first comma ("Nairobi, Kenya" gives
/// "Nairobi"), its whitespace and control characters collapsed to single spaces; `None` when the
/// pond has no place name.
pub fn local_town(location_name: &str) -> Option<String> {
    let town: String = location_name
        .split(',')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if town.is_empty() {
        return None;
    }
    let mut end = town.len().min(MAX_TOWN_BYTES);
    while !town.is_char_boundary(end) {
        end -= 1;
    }
    Some(town[..end].trim_end().to_string())
}

/// The town a search sends to Brave: only with local places on, and only a place name the
/// household stored. Never coordinates.
pub fn local_search_town(local_results: bool, location_name: &str) -> Option<String> {
    if local_results {
        local_town(location_name)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_spellings_are_the_contracts() {
        let all = [
            (WebSearchStatus::Ok, "ok"),
            (WebSearchStatus::NotConfigured, "not_configured"),
            (WebSearchStatus::Rejected, "rejected"),
            (WebSearchStatus::Quota, "quota"),
            (WebSearchStatus::Unverified, "unverified"),
        ];
        for (status, wire) in all {
            assert_eq!(status.as_str(), wire);
        }
    }

    #[test]
    fn brave_statuses_map_as_the_contract_says() {
        assert_eq!(
            WebSearchStatus::from_brave_http(200),
            Some(WebSearchStatus::Ok)
        );
        for code in [401, 403] {
            assert_eq!(
                WebSearchStatus::from_brave_http(code),
                Some(WebSearchStatus::Rejected)
            );
        }
        for code in [402, 429] {
            assert_eq!(
                WebSearchStatus::from_brave_http(code),
                Some(WebSearchStatus::Quota)
            );
        }
        for code in [400, 422, 500, 503] {
            assert_eq!(WebSearchStatus::from_brave_http(code), None, "{code}");
        }
    }

    #[test]
    fn llm_context_answers_map_as_the_fallback_table_says() {
        assert_eq!(
            LlmContextOutcome::from_http(200),
            LlmContextOutcome::Answered
        );
        assert_eq!(
            LlmContextOutcome::from_http(401),
            LlmContextOutcome::Rejected
        );
        assert_eq!(LlmContextOutcome::from_http(429), LlmContextOutcome::Quota);
        for code in [400, 402, 403, 404, 422, 500, 502, 503] {
            assert_eq!(
                LlmContextOutcome::from_http(code),
                LlmContextOutcome::Fallback,
                "{code}"
            );
        }
    }

    #[test]
    fn the_town_is_the_place_name_before_its_first_comma() {
        assert_eq!(local_town("Nairobi, Kenya").as_deref(), Some("Nairobi"));
        assert_eq!(local_town("  Kisumu  ").as_deref(), Some("Kisumu"));
        assert_eq!(
            local_town("Dar es\tSalaam").as_deref(),
            Some("Dar es Salaam")
        );
        assert_eq!(
            local_town("São Paulo, Brazil").as_deref(),
            Some("São Paulo")
        );
        assert_eq!(local_town("Nai\r\nrobi").as_deref(), Some("Nai robi"));
        assert_eq!(local_town(""), None);
        assert_eq!(local_town(" , Kenya"), None);
        let long = "é".repeat(80);
        let town = local_town(&long).unwrap();
        assert!(town.len() <= MAX_TOWN_BYTES, "{}", town.len());
    }

    #[test]
    fn no_town_is_sent_with_local_places_off() {
        assert_eq!(local_search_town(false, "Nairobi, Kenya"), None);
        assert_eq!(
            local_search_town(true, "Nairobi, Kenya").as_deref(),
            Some("Nairobi")
        );
        assert_eq!(local_search_town(true, ""), None);
    }
}
