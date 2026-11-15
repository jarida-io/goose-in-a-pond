//! The household notice a factory reset sends, in a handful of languages. Exact match against
//! `Profile.preferences["language"]`; unknown or absent falls back to English. Hand-rolled on
//! purpose — GIAP has not chosen an org-wide i18n library yet, and this notice shouldn't wait on
//! that decision.

/// The notice body for a factory reset, in `language`'s wording when known, English otherwise.
pub fn factory_reset_notice(language: &str) -> &'static str {
    match language {
        "sw" => "Pond imeondolewa kutoka kaya hii.",
        "es" => "El Pond ha sido eliminado de este hogar.",
        "fr" => "Le Pond a été retiré de ce foyer.",
        _ => "This Pond has been removed from the household.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_languages_get_their_own_wording() {
        assert!(factory_reset_notice("sw").contains("kaya"));
        assert!(factory_reset_notice("es").contains("hogar"));
        assert!(factory_reset_notice("fr").contains("foyer"));
    }

    #[test]
    fn unknown_or_absent_language_falls_back_to_english() {
        assert_eq!(
            factory_reset_notice("de"),
            "This Pond has been removed from the household."
        );
        assert_eq!(
            factory_reset_notice(""),
            "This Pond has been removed from the household."
        );
    }
}
