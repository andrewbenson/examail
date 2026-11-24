//! Fuzzy matching for the palette: a port of `command-score`.
//!
//! A faithful Rust port of `command-score` (<https://github.com/superhuman/command-score>, MIT
//! license), so rows rank exactly as the JavaScript library ranks them. It works on UTF-16 code
//! units, like the JavaScript string operations it mirrors.

use std::collections::HashMap;

/// Characters that start a "word" after punctuation (`/[\\\/_+.#"@\[\(\{&]/`).
const GAPS: &[u8] = br#"\/_+.#"@[({&"#;

/// How well `abbreviation` matches `string`, from 0 (no match) to 1 (exact).
///
/// Matching is case-insensitive, with a tiny penalty for each case mismatch. Contiguous matches
/// score highest, then matches at the start of a word (after a space or hyphen, then after
/// punctuation), then matches inside a word. An abbreviation that covers only a prefix of the
/// string caps at 0.99.
pub fn command_score(string: &str, abbreviation: &str) -> f64 {
    Matcher {
        string: Normalized::new(string),
        abbreviation: Normalized::new(abbreviation),
        memo: HashMap::new(),
    }
    .score(0, 0)
}

/// Original and normalized UTF-16 code units with an index map back to `original`.
struct Normalized {
    original: Vec<u16>,
    lower: Vec<u16>,
    /// For each code unit in `lower`, the corresponding index in `original`.
    /// Required because `str::to_lowercase()` can expand a character (e.g. `'İ'` -> `"i\u{307}"`).
    original_index: Vec<usize>,
}

impl Normalized {
    /// Lower-cases `text`, maps whitespace/hyphens to `' '`, and tracks each lowered unit's origin.
    fn new(text: &str) -> Self {
        let mut original = Vec::with_capacity(text.len());
        let mut lower = Vec::with_capacity(text.len());
        let mut original_index = Vec::with_capacity(text.len());
        let lowered = text.to_lowercase();
        let mut lower_chars = lowered.chars();
        let mut buf = [0u16; 2];

        for ch in text.chars() {
            let orig_start = original.len();
            let orig_units = ch.encode_utf16(&mut buf);
            original.extend_from_slice(orig_units);
            let orig_len = orig_units.len();

            let lower_start = lower.len();
            for _ in 0..ch.to_lowercase().count() {
                if let Some(low) = lower_chars.next() {
                    for unit in low.encode_utf16(&mut buf).iter().copied() {
                        lower.push(if is_space(unit) {
                            u16::from(b' ')
                        } else {
                            unit
                        });
                    }
                }
            }
            let lower_len = lower.len() - lower_start;
            if lower_len == orig_len {
                original_index.extend(orig_start..orig_start + lower_len);
            } else {
                original_index.resize(lower.len(), orig_start);
            }
        }

        let fallback = original.len().saturating_sub(1);
        for low in lower_chars {
            for unit in low.encode_utf16(&mut buf).iter().copied() {
                lower.push(if is_space(unit) {
                    u16::from(b' ')
                } else {
                    unit
                });
                original_index.push(fallback);
            }
        }

        Self {
            original,
            lower,
            original_index,
        }
    }

    fn original_unit(&self, lower_idx: usize) -> Option<u16> {
        let idx = *self.original_index.get(lower_idx)?;
        self.original.get(idx).copied()
    }
}

/// JavaScript's `/[\s-]/`: ECMAScript white space and line terminators, or a hyphen.
fn is_space(unit: u16) -> bool {
    matches!(
        unit,
        0x09..=0x0d | 0x20 | 0x2d | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

fn is_gap(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|byte| GAPS.contains(&byte))
}

struct Matcher {
    string: Normalized,
    abbreviation: Normalized,
    memo: HashMap<(usize, usize), f64>,
}

impl Matcher {
    /// The best score for matching `abbreviation.lower[a..]` against `string.lower[s..]`.
    fn score(&mut self, s: usize, a: usize) -> f64 {
        // JavaScript recurses past the end only in a case that can't arise (see the transposition
        // check below). `>=` keeps that impossible case from indexing out of bounds.
        if a >= self.abbreviation.lower.len() {
            return if s == self.string.lower.len() {
                1.0
            } else {
                0.99
            };
        }
        if let Some(&memoized) = self.memo.get(&(s, a)) {
            return memoized;
        }
        let wanted = self.abbreviation.lower[a];
        let mut best = 0.0;
        let mut found = self.index_of(wanted, s);
        while let Some(at) = found {
            let mut score = self.score(at + 1, a + 1);
            if score > best {
                if at == s {
                    // Contiguous: no penalty.
                } else if self.string.lower.get(at - 1).copied().is_some_and(is_gap) {
                    score *= 0.8;
                    let skipped = self.count_between(s, at - 1, is_gap);
                    if skipped > 0 && s > 0 {
                        score *= 0.999_f64.powf(skipped as f64);
                    }
                } else if self.string.lower.get(at - 1).copied().is_some_and(is_space) {
                    score *= 0.9;
                    let skipped = self.count_between(s, at - 1, is_space);
                    if skipped > 0 && s > 0 {
                        score *= 0.999_f64.powf(skipped as f64);
                    }
                } else {
                    score *= 0.3;
                    if s > 0 {
                        score *= 0.999_f64.powf((at - s) as f64);
                    }
                }
                if self.string.original_unit(at) != self.abbreviation.original_unit(a) {
                    score *= 0.9999;
                }
            }
            // Transpositions ("teh" for "the"). `None` stands for JavaScript's `charAt` past either
            // end, which returns "" (so two out-of-range characters compare equal).
            let before = at.checked_sub(1).and_then(|i| self.string.lower.get(i));
            let next = self.abbreviation.lower.get(a + 1);
            if (score < 0.1 && before == next) || (next == Some(&wanted) && before != Some(&wanted))
            {
                let transposed = self.score(at + 1, a + 2);
                if 0.1 * transposed > score {
                    score = 0.1 * transposed;
                }
            }
            if score > best {
                best = score;
            }
            found = self.index_of(wanted, at + 1);
        }
        self.memo.insert((s, a), best);
        best
    }

    /// How many of `lower[start..end]` satisfy `test` (clamping `end` to the string's end).
    fn count_between(&self, start: usize, end: usize, test: fn(u16) -> bool) -> usize {
        let end = end.min(self.string.lower.len());
        self.string
            .lower
            .get(start..end)
            .map_or(0, |units| units.iter().filter(|&&unit| test(unit)).count())
    }

    /// Where `unit` next occurs in the lower-cased string, from `from` on.
    fn index_of(&self, unit: u16, from: usize) -> Option<usize> {
        self.string
            .lower
            .get(from..)?
            .iter()
            .position(|&u| u == unit)
            .map(|i| i + from)
    }
}

#[cfg(test)]
mod tests {
    use super::command_score;

    /// `(string, abbreviation, score)` computed by the JavaScript `command-score`, for palette
    /// titles and typical queries.
    const VECTORS: &str = include_str!("../tests/command_score.json");

    #[test]
    fn matches_the_javascript_implementation() {
        let vectors: Vec<(String, String, f64)> =
            serde_json::from_str(VECTORS).expect("valid vectors");
        assert!(vectors.len() > 700);
        let mut adjusted = 0;
        for (string, abbreviation, expected) in vectors {
            // Upstream JS `command-score` indexes the un-lowered string with indices computed in
            // `toLowerCase()`. For `'İ'` (1 UTF-16 unit -> `"i\u{307}"`, 2 UTF-16 units), that
            // shifts lookups by 1: `"inb"` gets two spurious case penalties (`'b'!='n'`, `'u'!='b'`)
            // and `"u"` hits `s == string.len()` early (`1.0` instead of `0.99`) plus `'l'!='u'`.
            let expected = match (string.as_str(), abbreviation.as_str()) {
                ("İstanbul", "inb") => {
                    adjusted += 1;
                    0.99 * 0.3 * 0.999_f64.powf(4.0) * 0.9999
                }
                ("İstanbul", "u") => {
                    adjusted += 1;
                    0.99 * 0.3
                }
                _ => expected,
            };
            let actual = command_score(&string, &abbreviation);
            assert!(
                (actual - expected).abs() < 1e-12,
                "command_score({string:?}, {abbreviation:?}) = {actual}, expected {expected}"
            );
        }
        assert_eq!(adjusted, 2);
    }

    #[test]
    fn ranks_like_the_readme() {
        assert_eq!(command_score("hello", "hello"), 1.0);
        assert!(command_score("hello world", "hw") > command_score("hello world", "ho"));
        assert_eq!(command_score("hello", "z"), 0.0);
    }

    #[test]
    fn expanding_lowercase_keeps_bounds_and_case_aligned() {
        // `'İ'` (U+0130, 1 UTF-16 code unit) expands to `"i\u{307}"` (2 UTF-16 code units).
        // Non-matching trailing characters must fail instead of terminating early at `1.0`.
        assert_eq!(command_score("İx", "İz"), 0.0);
        assert_eq!(command_score("İ", "İz"), 0.0);
        assert_eq!(command_score("İİa", "İİb"), 0.0);
        assert_eq!(command_score("ix", "İ"), 0.0);

        // Exact match scores 1.0; case mismatch after `'İ'` applies the single case penalty.
        assert_eq!(command_score("İ", "İ"), 1.0);
        assert_eq!(command_score("İx", "İx"), 1.0);
        assert!((command_score("İx", "İX") - 0.9999).abs() < 1e-12);
        assert!((command_score("İx", "İ") - 0.99).abs() < 1e-12);

        // Word-start detection after `'İ'` uses the normalized index, not a shifted one.
        assert!((command_score("İ foo", "f") - command_score("i foo", "f")).abs() < 1e-12);
        assert!((command_score("İ/foo", "f") - command_score("i/foo", "f")).abs() < 1e-12);

        // Matching `'İ'` against its decomposed lowercase form succeeds with case penalties.
        let decomposed = command_score("İ", "i\u{307}");
        assert!(decomposed > 0.999 && decomposed < 1.0);

        // Surrogate pairs (non-BMP) and Greek final sigma keep 1-to-1 UTF-16 alignment.
        assert_eq!(command_score("𐐀", "𐐀"), 1.0);
        assert!((command_score("𐐀", "𐐨") - 0.9999).abs() < 1e-12);
        assert!((command_score("ΟΣ", "ος") - 0.9999 * 0.9999).abs() < 1e-12);
    }
}
