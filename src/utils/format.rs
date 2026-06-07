use crate::constants::{ELLIPSIS, ELLIPSIS_DISPLAY_WIDTH, ELLIPSIS_LEN};
use crate::discord::autocomplete::{AUTOCOMPLETE_MAX_LENGTH, AUTOCOMPLETE_SEPARATOR, AUTOCOMPLETE_SEPARATOR_LEN};



pub fn build_autocomplete_display(mut to_display: Vec<String>) -> String {
    // Build a display name
    let content_max_length = AUTOCOMPLETE_MAX_LENGTH - (AUTOCOMPLETE_SEPARATOR_LEN * to_display.len()) + 1;

    let mut lens: Vec<usize> = to_display
        .iter()
        .map(|n| n.len())
        .collect();
    let total_len: usize = lens.iter().sum();
    let mut excess = total_len.saturating_sub(content_max_length);

    // truncate each as needed
    while excess > 0 {
        // pick the index of the longest field
        let (max_idx, &max_len) = lens
            .iter()
            .enumerate()
            .max_by_key(|&(_, &l)| l)
            .expect("lens vector should never be empty when truncating autocomplete display");

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

        // back up to a valid UTF-8 boundary
        let mut adjust = new_len;
        while adjust > 0 && !s.is_char_boundary(adjust) {
            adjust -= 1;
        }
        s.truncate(adjust);

        // append ellipsis if we cut something
        if needs_ellipsis {
            s.push_str(ELLIPSIS);
            lens[max_idx] = adjust + ELLIPSIS_LEN;
        } else {
            lens[max_idx] = adjust;
        }

        excess = excess.saturating_sub(chop);
    }

    to_display.join(AUTOCOMPLETE_SEPARATOR)

}

pub fn lightweight_trim(mut choice: String, max_width: usize) -> String {
    if max_width <= ELLIPSIS_DISPLAY_WIDTH {
        return ELLIPSIS.to_string();
    }

    if choice.len() > max_width {
        let cutoff = max_width - 1;
        let safe_cutoff = choice
            .char_indices()
            .take_while(|(idx, _)| *idx <= cutoff)
            .map(|(idx, _)| idx)
            .last()
            .unwrap_or(0);

        choice.truncate(safe_cutoff);
        choice.push_str(ELLIPSIS);
    }

    choice
}