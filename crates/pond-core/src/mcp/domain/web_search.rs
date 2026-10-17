//! Web search as the pond offers it: the backend's address and the states the settings screens
//! report. The key lives in the secret store under [`super::tool_group::BRAVE_SEARCH_API_KEY`].

use std::time::Duration;

/// The backend's name, as `GET /api/v1/web-search` reports it.
pub const WEB_SEARCH_PROVIDER: &str = "brave";

/// Origin of the Brave Search API.
pub const BRAVE_API_BASE: &str = "https://api.search.brave.com";

/// Brave's web search endpoint, under [`BRAVE_API_BASE`].
pub const BRAVE_WEB_SEARCH_PATH: &str = "/res/v1/web/search";

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
}
