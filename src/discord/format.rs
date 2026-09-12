use crate::discord::constants::{
    AUTOCOMPLETE_MAX_LENGTH, AUTOCOMPLETE_SEPARATOR, AUTOCOMPLETE_SEPARATOR_LEN, ELLIPSIS,
    ELLIPSIS_DISPLAY_WIDTH, ELLIPSIS_LEN,
};

pub fn build_autocomplete_display(mut to_display: Vec<String>) -> String {
    // Build a display name
    let content_max_length =
        AUTOCOMPLETE_MAX_LENGTH - (AUTOCOMPLETE_SEPARATOR_LEN * to_display.len()) + 1;

    let mut lens: Vec<usize> = to_display
        .iter()
        .map(|value| value.chars().count())
        .collect();
    let total_len: usize = lens.iter().sum();
    let mut excess = total_len.saturating_sub(content_max_length);

    // truncate each as needed
    while excess > 0 {
        // pick the index of the longest field
        let Some((max_idx, &max_len)) = lens.iter().enumerate().max_by_key(|&(_, &l)| l) else {
            break;
        };

        // decide how many bytes to chop
        let chop = excess.min(max_len);
        let mut new_len = max_len.saturating_sub(chop);

        // reserve room for ellipsis if we're actually cutting
        let needs_ellipsis = new_len < max_len;
        if needs_ellipsis && new_len > ELLIPSIS_LEN {
            new_len = new_len.saturating_sub(ELLIPSIS_LEN);
        }

        // get the mutable String reference
        let s: &mut String = &mut to_display[max_idx];

        let byte_len = s
            .char_indices()
            .nth(new_len)
            .map_or(s.len(), |(index, _)| index);
        s.truncate(byte_len);

        // append ellipsis if we cut something
        if needs_ellipsis {
            s.push_str(ELLIPSIS);
            lens[max_idx] = new_len + ELLIPSIS_LEN;
        } else {
            lens[max_idx] = new_len;
        }

        excess = excess.saturating_sub(chop);
    }

    to_display.join(AUTOCOMPLETE_SEPARATOR)
}

pub fn lightweight_trim(mut choice: String, max_width: usize) -> String {
    if max_width <= ELLIPSIS_DISPLAY_WIDTH {
        return ELLIPSIS.to_string();
    }

    if choice.chars().count() > max_width {
        let safe_cutoff = choice
            .char_indices()
            .nth(max_width - ELLIPSIS_DISPLAY_WIDTH)
            .map_or(choice.len(), |(index, _)| index);

        choice.truncate(safe_cutoff);
        choice.push_str(ELLIPSIS);
    }

    choice
}

#[cfg(test)]
mod tests {
    use super::{build_autocomplete_display, lightweight_trim};

    #[test]
    fn trims_unicode_by_character_count() {
        assert_eq!(lightweight_trim("ééé".to_owned(), 2), "é…");
    }

    #[test]
    fn autocomplete_display_respects_unicode_character_count() {
        let display = build_autocomplete_display(vec!["é".repeat(110)]);
        assert!(display.chars().count() <= 100);
        assert!(display.ends_with('…'));
    }

    #[test]
    fn trim_leaves_short_choices_unchanged() {
        assert_eq!(lightweight_trim("short".to_owned(), 10), "short");
    }

    #[test]
    fn trim_returns_ellipsis_when_width_cannot_hold_content() {
        assert_eq!(lightweight_trim("anything".to_owned(), 0), "…");
        assert_eq!(lightweight_trim("anything".to_owned(), 1), "…");
    }

    #[test]
    fn trim_honours_exact_width() {
        assert_eq!(lightweight_trim("abcd".to_owned(), 4), "abcd");
        assert_eq!(lightweight_trim("abcde".to_owned(), 4), "abc…");
    }

    #[test]
    fn autocomplete_display_joins_fields_without_truncating_short_values() {
        assert_eq!(
            build_autocomplete_display(vec!["Title".into(), "Artist".into(), "Origin".into()]),
            "Title | Artist | Origin"
        );
    }

    #[test]
    fn autocomplete_display_handles_no_fields() {
        assert!(build_autocomplete_display(Vec::new()).is_empty());
    }

    #[test]
    fn autocomplete_display_trims_multiple_long_fields_to_the_limit() {
        let display = build_autocomplete_display(vec!["a".repeat(80), "b".repeat(80)]);
        assert!(display.chars().count() <= 100);
        assert!(display.contains(" | "));
        assert!(display.matches('…').count() >= 1);
    }
}
