//! Voice text-expansion snippets. Since the ASR only ever produces spoken words (there is no way to
//! "type" a `/command`), snippets are triggered by **speech**: a keyword (default "insert") followed
//! by a snippet's trigger phrase. When the words spoken after the keyword match a snippet, the whole
//! "`<keyword> <trigger>`" span is replaced with the snippet's exact expansion; otherwise the text is
//! left untouched (so "insert coffee", with no "coffee" snippet, stays "insert coffee").
//!
//! This is a deterministic pass (never the LLM) so expansions are pasted **verbatim**, and it runs
//! *after* the formatter so the model can't reflow the expansion. Trigger matching is tolerant
//! (case/punctuation-insensitive, with a Jaro-Winkler fallback) to absorb small ASR variations.

use strsim::jaro_winkler;

use crate::config::Snippet;

/// A prepared snippet: its trigger split into normalized word tokens plus the verbatim expansion.
struct Prepared {
    tokens: Vec<String>,
    joined: String,
    expansion: String,
}

/// Compiled snippet expander. Cheap to build; rebuilt when the list or keyword changes.
pub struct Expander {
    keyword: String,
    snippets: Vec<Prepared>,
}

impl Expander {
    /// Compile from the keyword and snippet list. Blank triggers/expansions are dropped; snippets
    /// are ordered longest-trigger-first so the most specific match wins.
    pub fn new(keyword: &str, snippets: &[Snippet]) -> Expander {
        let keyword = normalize(keyword);
        let keyword = if keyword.is_empty() { "snippet".to_string() } else { keyword };
        let mut prepared: Vec<Prepared> = snippets
            .iter()
            .filter(|s| !s.trigger.trim().is_empty() && !s.expansion.is_empty())
            .map(|s| {
                let tokens: Vec<String> = s
                    .trigger
                    .split_whitespace()
                    .map(normalize)
                    .filter(|t| !t.is_empty())
                    .collect();
                Prepared {
                    joined: tokens.join(" "),
                    tokens,
                    expansion: s.expansion.clone(),
                }
            })
            .filter(|p| !p.tokens.is_empty())
            .collect();
        prepared.sort_by(|a, b| b.tokens.len().cmp(&a.tokens.len()));
        Expander { keyword, snippets: prepared }
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// Expand any "`<keyword> <trigger>`" occurrences in `text`. Words are matched case- and
    /// punctuation-insensitively; punctuation attached to the keyword or the last trigger word is
    /// preserved around the inserted expansion.
    pub fn apply(&self, text: &str) -> String {
        if self.snippets.is_empty() {
            return text.to_string();
        }
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let mut out: Vec<String> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            if normalize(tokens[i]) == self.keyword {
                if let Some((prep, k)) = self.match_at(&tokens, i + 1) {
                    let (lead, _) = split_affixes(tokens[i]);
                    let (_, trail) = split_affixes(tokens[i + k]);
                    out.push(format!("{lead}{}{trail}", prep.expansion));
                    i += k + 1;
                    continue;
                }
            }
            out.push(tokens[i].to_string());
            i += 1;
        }
        out.join(" ")
    }

    /// Try to match a snippet trigger starting at token index `start`. Returns the snippet and its
    /// token count on success.
    fn match_at(&self, tokens: &[&str], start: usize) -> Option<(&Prepared, usize)> {
        for prep in &self.snippets {
            let k = prep.tokens.len();
            if start + k > tokens.len() {
                continue;
            }
            let window: Vec<String> = tokens[start..start + k].iter().map(|t| normalize(t)).collect();
            let per_token_equal = window == prep.tokens;
            let close = jaro_winkler(&window.join(" "), &prep.joined) >= 0.90;
            if per_token_equal || close {
                return Some((prep, k));
            }
        }
        None
    }
}

/// Lowercase, alphanumerics only (drops surrounding/embedded punctuation for comparison).
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Split a raw token into (leading punctuation, trailing punctuation) around its alphanumeric core.
fn split_affixes(tok: &str) -> (&str, &str) {
    let start = tok.find(|c: char| c.is_alphanumeric());
    let Some(start) = start else { return ("", "") };
    let end = tok
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(tok.len());
    (&tok[..start], &tok[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snip(trigger: &str, expansion: &str) -> Snippet {
        Snippet { trigger: trigger.into(), expansion: expansion.into() }
    }

    fn expander(pairs: &[(&str, &str)]) -> Expander {
        let snippets: Vec<Snippet> = pairs.iter().map(|(t, e)| snip(t, e)).collect();
        Expander::new("insert", &snippets)
    }

    #[test]
    fn expands_a_matching_snippet() {
        let e = expander(&[("address", "123 Main St")]);
        assert_eq!(e.apply("insert address"), "123 Main St");
    }

    #[test]
    fn leaves_non_snippet_after_keyword_untouched() {
        // The user's requirement: "insert coffee" with no "coffee" snippet stays as-is.
        let e = expander(&[("address", "123 Main St")]);
        assert_eq!(e.apply("insert coffee please"), "insert coffee please");
    }

    #[test]
    fn expands_multiword_trigger_inline_and_keeps_surrounding_text() {
        let e = expander(&[("work email", "me@work.com")]);
        assert_eq!(e.apply("please insert work email today"), "please me@work.com today");
    }

    #[test]
    fn preserves_trailing_punctuation() {
        let e = expander(&[("address", "123 Main St")]);
        assert_eq!(e.apply("Insert address."), "123 Main St.");
    }

    #[test]
    fn tolerates_small_asr_variation() {
        let e = expander(&[("work email", "me@work.com")]);
        // "e-mail" normalizes to "email".
        assert_eq!(e.apply("insert work e-mail"), "me@work.com");
    }

    #[test]
    fn longest_trigger_wins() {
        let e = expander(&[("email", "SHORT"), ("email signature", "LONG BLOCK")]);
        assert_eq!(e.apply("insert email signature"), "LONG BLOCK");
    }

    #[test]
    fn keyword_alone_or_empty_list_is_noop() {
        let e = expander(&[("address", "123 Main St")]);
        assert_eq!(e.apply("insert"), "insert");
        let empty = Expander::new("insert", &[]);
        assert_eq!(empty.apply("insert address"), "insert address");
    }
}
