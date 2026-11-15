//! What a tool asked to set something for later really did, carried in `CallToolResult._meta`
//! beside the words the model reads. The turn keeps it, the screens draw it, and a failed set is
//! said plainly whatever the model writes.

use serde_json::{Map, Value};

use crate::user_data::domain::message_note::Receipt;

/// The `_meta` key a set-for-later result carries; its value is a [`Receipt`].
pub const OUTCOME_META_KEY: &str = "giap/outcome";

/// `meta` with the receipt added, keeping whatever else it held.
pub fn with_outcome(meta: Option<Map<String, Value>>, receipt: &Receipt) -> Map<String, Value> {
    let mut meta = meta.unwrap_or_default();
    if let Ok(value) = serde_json::to_value(receipt) {
        meta.insert(OUTCOME_META_KEY.to_string(), value);
    }
    meta
}

/// The receipt a result's `_meta` carries; `None` for a tool that sets nothing.
pub fn receipt_from_meta(meta: &Map<String, Value>) -> Option<Receipt> {
    serde_json::from_value(meta.get(OUTCOME_META_KEY)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::domain::transient_result::{keep_from_meta, transient_meta};

    #[test]
    fn a_receipt_rides_the_meta_beside_what_was_there() {
        let receipt = Receipt::not_set(
            "timer",
            "41 days",
            "I could not tell how long it should run.",
        );
        let meta = with_outcome(Some(transient_meta("kept")), &receipt);
        assert_eq!(receipt_from_meta(&meta), Some(receipt));
        assert_eq!(keep_from_meta(&meta).as_deref(), Some("kept"));
        assert_eq!(receipt_from_meta(&Map::new()), None);
    }
}
