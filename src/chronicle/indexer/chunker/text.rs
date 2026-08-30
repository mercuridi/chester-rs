use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

pub(super) fn sentence_ranges(text: &str) -> Vec<Range<usize>> {
    text.split_sentence_bound_indices()
        .map(|(start, sentence)| start..start + sentence.len())
        .collect()
}

pub(super) fn word_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut in_word = false;

    for (index, character) in text.char_indices() {
        if !in_word && !character.is_whitespace() {
            in_word = true;
        } else if in_word && character.is_whitespace() {
            ranges.push(start..index);
            start = index;
            in_word = false;
        }
    }

    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}
