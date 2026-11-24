//! Grapheme clusters, which the caret moves over and Delete removes whole, as Chromium does
//! (UAX #29's extended clusters: flags, emoji sequences, Hangul syllables, Indic conjuncts, a
//! letter and its marks). Offsets are in characters, as [`crate::Pos`] counts them.

use std::sync::LazyLock;

use icu_segmenter::GraphemeClusterSegmenter;
use regex::Regex;

/// What Backspace deletes whole rather than one character at a time: emoji sequences, flags,
/// keycaps (Blink's `BackspaceStateMachine`).
static EMOJI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Extended_Pictographic}\p{gcb=RI}\x{20E3}\x{FE0F}]").expect("valid regex")
});

/// Extended grapheme clusters of `text` as `(start, end)` in characters, paired with each
/// cluster's slice.
fn cluster_spans(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut boundaries = GraphemeClusterSegmenter::new().segment_str(text);
    let mut byte_start = boundaries.next().unwrap_or(0);
    let mut char_pos = 0;
    boundaries.map(move |byte_end| {
        let slice = &text[byte_start..byte_end];
        let start = char_pos;
        char_pos += slice.chars().count();
        byte_start = byte_end;
        (start, char_pos, slice)
    })
}

/// The clusters of `text`, as `(start, end)` in characters.
pub(crate) fn clusters(text: &str) -> Vec<(usize, usize)> {
    cluster_spans(text)
        .map(|(start, end, _)| (start, end))
        .collect()
}

/// The end of the cluster after `offset` (Delete, →).
pub(crate) fn next(text: &str, offset: usize) -> usize {
    cluster_spans(text)
        .map(|(_, end, _)| end)
        .find(|end| *end > offset)
        .unwrap_or(offset)
}

/// The start of the cluster before `offset` (←).
pub(crate) fn previous(text: &str, offset: usize) -> usize {
    cluster_spans(text)
        .map(|(start, _, _)| start)
        .take_while(|start| *start < offset)
        .last()
        .unwrap_or(0)
}

/// Where Backspace at `offset` deletes from. That's the start of an emoji cluster, or else one
/// character back, so a letter keeps its base when its accent goes (Chromium's backward deletion).
pub(crate) fn backspace(text: &str, offset: usize) -> usize {
    let Some((start, _, slice)) = cluster_spans(text)
        .take_while(|(start, _, _)| *start < offset)
        .last()
    else {
        return 0;
    };
    let head = match slice.char_indices().nth(offset - start) {
        Some((byte, _)) => &slice[..byte],
        None => slice,
    };
    if EMOJI.is_match(head) {
        start
    } else {
        offset.saturating_sub(1)
    }
}

/// `offset` moved off the inside of a cluster to the nearer of its ends, or to the start on a tie.
pub(crate) fn snap(text: &str, offset: usize) -> usize {
    match cluster_spans(text).find(|(start, end, _)| *start < offset && offset < *end) {
        Some((start, end, _)) if end - offset < offset - start => end,
        Some((start, _, _)) => start,
        None => offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_hold_what_shows_as_one_character() {
        let text =
            "e\u{301}🇺🇸👨\u{200d}👩\u{200d}👧👍🏽1\u{fe0f}\u{20e3}\u{1100}\u{1161}\u{915}\u{93f}a";
        let lengths: Vec<usize> = clusters(text)
            .into_iter()
            .map(|(start, end)| end - start)
            .collect();
        assert_eq!(lengths, [2, 2, 5, 2, 3, 2, 2, 1]);
    }

    #[test]
    fn indic_conjuncts_stay_together_as_one_cluster() {
        // Devanagari क्ष (Ka + Virama + Ssa, UAX #29 GB9c).
        let text = "a\u{0915}\u{094D}\u{0937}b";
        assert_eq!(clusters(text), [(0, 1), (1, 4), (4, 5)]);
        assert_eq!(next(text, 1), 4);
        assert_eq!(previous(text, 4), 1);
        assert_eq!(snap(text, 2), 1);
        assert_eq!(snap(text, 3), 4);
        assert_eq!(backspace(text, 4), 3);
    }

    #[test]
    fn backspace_takes_emoji_whole_and_marks_alone() {
        assert_eq!(backspace("a🇺🇸", 3), 1);
        assert_eq!(backspace("a👍🏽", 3), 1);
        assert_eq!(backspace("ae\u{301}", 3), 2, "the accent goes, the e stays");
        assert_eq!(backspace("ab", 2), 1);
    }

    #[test]
    fn moves_and_snaps_by_cluster() {
        let text = "xe\u{301}🇺🇸";
        assert_eq!(next(text, 1), 3);
        assert_eq!(next(text, 3), 5);
        assert_eq!(previous(text, 5), 3);
        assert_eq!(previous(text, 3), 1);
        assert_eq!(snap(text, 2), 1);
        assert_eq!(snap(text, 4), 3);
        assert_eq!(snap(text, 3), 3);
    }
}
