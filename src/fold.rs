//! Search compares text folded: lowercased, with the accents of Latin and
//! Greek letters dropped (é → e, ç → c, ά → α, ΐ → ι) and final sigma
//! written as sigma, so "οδος" finds "Οδός" and "cafe" finds "Café".
//! Other scripts are only lowercased: there an accent can make a different
//! letter (Cyrillic й is not и).

use std::collections::HashMap;
use std::sync::OnceLock;

/// Accents left as separate characters (e + U+0301) are dropped too.
fn is_accent(c: char) -> bool {
    ('\u{0300}'..='\u{036F}').contains(&c)
}

/// Latin and Greek letters, where an accent is just an accent.
fn accent_folds(c: char) -> bool {
    matches!(c as u32, 0x00C0..=0x024F | 0x0370..=0x03FF | 0x1E00..=0x1FFF)
}

/// Append `c`, folded, to `out`.
pub fn push_folded(c: char, out: &mut String) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
        return;
    }
    for lc in c.to_lowercase() {
        if is_accent(lc) {
            continue;
        }
        if lc == 'ς' {
            out.push('σ');
        } else if accent_folds(lc) {
            unicode_normalization::char::decompose_canonical(lc, |d| {
                if !is_accent(d) {
                    out.push(d);
                }
            });
        } else {
            out.push(lc);
        }
    }
}

/// `s` folded.
pub fn fold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_folded(c, &mut out);
    }
    out
}

/// `s` folded, and for each byte of the result (plus one past its end) the
/// byte offset in `s` of the char it came from.
pub fn fold_with_origin(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len() + 1);
    for (i, c) in s.char_indices() {
        push_folded(c, &mut out);
        origin.resize(out.len(), i);
    }
    origin.push(s.len());
    (out, origin)
}

/// A regex matching `folded` (folded text) wherever the original has any
/// case or accents: each letter that accented ones fold to stands for all
/// of them, and may carry separate accent marks.
pub fn unfolding_pattern(folded: &str) -> String {
    let variants = variants();
    let mut pattern = String::with_capacity(folded.len() * 2);
    for c in folded.chars() {
        match variants.get(&c) {
            Some(more) => {
                pattern.push_str("(?:[");
                pattern.push_str(&regex::escape(&c.to_string()));
                for v in more {
                    pattern.push_str(&regex::escape(&v.to_string()));
                }
                pattern.push_str("][\\x{300}-\\x{36F}]*)");
            }
            None => pattern.push_str(&regex::escape(&c.to_string())),
        }
    }
    pattern
}

/// For each folded letter, the other letters that fold to it.
fn variants() -> &'static HashMap<char, Vec<char>> {
    static VARIANTS: OnceLock<HashMap<char, Vec<char>>> = OnceLock::new();
    VARIANTS.get_or_init(|| {
        let mut map: HashMap<char, Vec<char>> = HashMap::new();
        let ranges = [0x00C0..=0x024F, 0x0370..=0x03FF, 0x1E00..=0x1FFF];
        for c in ranges.into_iter().flatten().filter_map(char::from_u32) {
            let folded = fold(&c.to_string());
            let mut chars = folded.chars();
            if let (Some(f), None) = (chars.next(), chars.next()) {
                // Capitals too: case-insensitive matching doesn't take `i`
                // to İ, or ϒ to ϓ and ϔ.
                if f != c {
                    map.entry(f).or_default().push(c);
                }
            }
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_accents_and_final_sigma() {
        assert_eq!(fold("Οδός ΟΔΟΣ Café NAÏVE"), "οδοσ οδοσ cafe naive");
        assert_eq!(fold("ΐ ϋ Ώ"), "ι υ ω");
        assert_eq!(fold("e\u{301}te"), "ete", "separate accent marks too");
        assert_eq!(fold("İstanbul"), "istanbul");
        assert_eq!(fold("ЙОД"), "йод", "Cyrillic keeps its letters");
        assert_eq!(fold("नमस्ते"), "नमस्ते", "and so does Devanagari");
        assert_eq!(fold("plain ASCII"), "plain ascii");
    }

    #[test]
    fn origin_maps_each_folded_byte_back() {
        let (folded, origin) = fold_with_origin("Αθήνα é");
        assert_eq!(folded, "αθηνα e");
        assert_eq!(origin.len(), folded.len() + 1);
        // "η" (2 bytes) comes from "ή" at byte 4.
        assert_eq!(&origin[4..6], &[4, 4]);
        assert_eq!(*origin.last().unwrap(), "Αθήνα é".len());
    }

    #[test]
    fn unfolding_pattern_matches_the_written_forms() {
        let re = regex::RegexBuilder::new(&unfolding_pattern(&fold("Οδός Café")))
            .case_insensitive(true)
            .build()
            .unwrap();
        for written in ["Οδός Café", "ΟΔΟΣ CAFE", "οδος cafe\u{301}"] {
            assert!(re.is_match(written), "{}", written);
        }
        assert!(!re.is_match("οδοι cafe"));
        let re = regex::RegexBuilder::new(&unfolding_pattern(&fold("istanbul")))
            .case_insensitive(true)
            .build()
            .unwrap();
        assert!(re.is_match("İstanbul"), "capitals that don't lowercase to their letter");
    }
}
