//! COCO class → GIAP event label. COCO has no "package", so luggage classes stand in.

/// GIAP label for a COCO class, or `None` (plain `"motion"`) when not home-relevant.
pub fn coco_to_giap_label(class_idx: usize) -> Option<&'static str> {
    match class_idx {
        0 => Some("person"),
        // bird, cat, dog
        14..=16 => Some("pet"),
        // backpack, handbag, suitcase — the "package" proxy classes
        24 | 26 | 28 => Some("package"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_home_relevant_classes_and_ignores_the_rest() {
        assert_eq!(coco_to_giap_label(0), Some("person"));
        assert_eq!(coco_to_giap_label(15), Some("pet")); // cat
        assert_eq!(coco_to_giap_label(16), Some("pet")); // dog
        assert_eq!(coco_to_giap_label(28), Some("package")); // suitcase
        assert_eq!(coco_to_giap_label(2), None); // car — not home-relevant
        assert_eq!(coco_to_giap_label(79), None); // toothbrush
    }
}
