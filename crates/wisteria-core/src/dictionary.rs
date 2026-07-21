//! Deterministic dictionary correction — the always-on half of the hybrid custom-vocabulary
//! feature. It replaces miscased or phonetically-near occurrences of the user's custom words
//! (names, jargon, brands) with their exact spelling, and runs even when the LLM formatter is off.
//!
//! It is deliberately **conservative** to avoid false positives (turning an ordinary word into
//! someone's name): it only touches **single-token** terms and only replaces a word when it either
//! matches case-insensitively (a pure casing fix) or is both phonetically equal (Soundex) *and*
//! spelled very similarly (Jaro-Winkler). Multi-word terms and context-dependent, badly-garbled
//! mishearings are left to the LLM pass, which also receives the same term list (see [`crate::format`]).

use strsim::jaro_winkler;

/// A prepared single-token dictionary term.
struct Term {
    /// Exact spelling to emit.
    canonical: String,
    /// Lowercase alphanumeric comparison key.
    key: String,
    /// Soundex phonetic code of `key`.
    soundex: String,
}

/// A compiled matcher over the user's dictionary. Cheap to build; rebuilt when the list changes.
pub struct Matcher {
    terms: Vec<Term>,
}

impl Matcher {
    /// Compile `entries` (canonical spellings). Multi-word and 1-char entries are ignored by the
    /// deterministic pass (the LLM handles those).
    pub fn new(entries: &[String]) -> Matcher {
        let terms = entries
            .iter()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty() && !e.contains(char::is_whitespace))
            .filter_map(|e| {
                let key = normalize(e);
                if key.len() < 2 {
                    return None;
                }
                Some(Term {
                    canonical: e.to_string(),
                    soundex: soundex(&key),
                    key,
                })
            })
            .collect();
        Matcher { terms }
    }

    /// True when there's nothing to do (so callers can skip the pass entirely).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Correct `text` word-by-word. Whitespace is normalized to single spaces (this runs on the
    /// single-line raw ASR transcript, before the formatter re-flows it).
    pub fn apply(&self, text: &str) -> String {
        if self.terms.is_empty() {
            return text.to_string();
        }
        text.split_whitespace()
            .map(|tok| self.fix_token(tok))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Fix a single whitespace-delimited token, preserving surrounding punctuation and a trailing
    /// possessive (`Aarjav's`).
    fn fix_token(&self, tok: &str) -> String {
        let Some(start) = tok.find(|c: char| c.is_alphanumeric()) else {
            return tok.to_string();
        };
        let end = tok
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_alphanumeric())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(tok.len());
        let prefix = &tok[..start];
        let mut core = &tok[start..end];
        let suffix = &tok[end..];

        // Peel a trailing possessive so "Aarjav's" still matches "Aarjav".
        let mut poss = "";
        for p in ["'s", "\u{2019}s"] {
            if let Some(base) = core.strip_suffix(p) {
                if !base.is_empty() {
                    poss = &core[base.len()..];
                    core = base;
                    break;
                }
            }
        }

        match self.best_match(core) {
            Some(canon) => format!("{prefix}{canon}{poss}{suffix}"),
            None => tok.to_string(),
        }
    }

    /// Return the canonical spelling to replace `core` with, or `None` to leave it alone.
    fn best_match(&self, core: &str) -> Option<&str> {
        let norm = normalize(core);
        if norm.len() < 2 {
            return None;
        }
        let sx = soundex(&norm);
        let mut best: Option<(f64, &str)> = None;
        for t in &self.terms {
            if norm == t.key {
                // Case-insensitive hit: only "correct" if the casing actually differs.
                return (core != t.canonical).then_some(t.canonical.as_str());
            }
            let jw = jaro_winkler(&norm, &t.key);
            let matched = (sx == t.soundex && jw >= 0.85) || jw >= 0.93;
            if matched && best.map_or(true, |(b, _)| jw > b) {
                best = Some((jw, t.canonical.as_str()));
            }
        }
        best.map(|(_, c)| c)
    }
}

/// Lowercase + keep only alphanumerics (drops apostrophes, dots, slashes) for comparison.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Classic Soundex phonetic code (letter + 3 digits). Crude but a useful coarse phonetic filter
/// when combined with Jaro-Winkler. Non-ASCII/empty input yields an empty code (never matches).
fn soundex(s: &str) -> String {
    let digit = |c: char| -> Option<u8> {
        match c.to_ascii_lowercase() {
            'b' | 'f' | 'p' | 'v' => Some(b'1'),
            'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some(b'2'),
            'd' | 't' => Some(b'3'),
            'l' => Some(b'4'),
            'm' | 'n' => Some(b'5'),
            'r' => Some(b'6'),
            _ => None,
        }
    };
    let mut chars = s.chars().filter(|c| c.is_ascii_alphabetic());
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(4);
    out.push(first.to_ascii_uppercase());
    let mut last = digit(first);
    for c in chars {
        let d = digit(c);
        if let Some(dd) = d {
            if Some(dd) != last {
                out.push(dd as char);
                if out.len() >= 4 {
                    break;
                }
            }
        }
        // Vowels break adjacency (so "tt" split by a vowel would count twice); h/w/y do not.
        last = if matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u') {
            None
        } else {
            d.or(last)
        };
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(words: &[&str]) -> Matcher {
        Matcher::new(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn fixes_casing_of_known_terms() {
        let m = matcher(&["Kubernetes", "oklch", "Wisteria"]);
        assert_eq!(m.apply("we deployed kubernetes with oklch colors"),
                   "we deployed Kubernetes with oklch colors");
    }

    #[test]
    fn preserves_punctuation_and_possessive() {
        let m = matcher(&["Aarjav"]);
        // Trailing comma + possessive kept around the corrected word.
        assert_eq!(m.apply("that is aarjav, and aarjav's repo"),
                   "that is Aarjav, and Aarjav's repo");
    }

    #[test]
    fn corrects_close_phonetic_misspelling() {
        let m = matcher(&["Kubernetes"]);
        // A near-miss the ASR might produce.
        assert_eq!(m.apply("kuberneties rocks"), "Kubernetes rocks");
    }

    #[test]
    fn leaves_unrelated_words_alone() {
        let m = matcher(&["Aarjav", "Kubernetes"]);
        let input = "the quick brown fox jumped over the lazy dog";
        assert_eq!(m.apply(input), input);
    }

    #[test]
    fn multiword_and_tiny_entries_are_ignored_by_this_pass() {
        // Multi-word ("async await") and 1-char entries aren't handled here (LLM does those).
        let m = matcher(&["async await", "x"]);
        assert!(m.is_empty());
    }

    #[test]
    fn empty_dictionary_is_a_noop() {
        let m = matcher(&[]);
        assert_eq!(m.apply("hello there"), "hello there");
    }

    #[test]
    fn soundex_basic_codes() {
        assert_eq!(soundex("robert"), "R163");
        assert_eq!(soundex("rupert"), "R163");
        assert_eq!(soundex(""), "");
    }
}
