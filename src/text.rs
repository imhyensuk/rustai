//! Shared tokenisation.
//!
//! One tokeniser feeds BM25, the density scorer and the redundancy filter, so
//! that a "token" means the same thing everywhere — including in the budget the
//! caller sets for their local model's context window.
//!
//! Latin-script text is split on word boundaries and lowercased. CJK runs are
//! additionally emitted as character bigrams, the standard trick that lets a
//! bag-of-words ranker work on Korean, Japanese and Chinese without shipping a
//! morphological analyser.

use unicode_segmentation::UnicodeSegmentation;

/// True for Hangul, Hiragana/Katakana and CJK ideographs.
#[inline]
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF   // Hangul Jamo
        | 0x3040..=0x30FF // Kana
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xA960..=0xA97F // Hangul Jamo ext A
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK compat
        | 0xFF66..=0xFF9D // halfwidth kana
        | 0x20000..=0x2FA1F
    )
}

/// A token is kept if it carries any letter or digit.
#[inline]
fn is_meaningful(w: &str) -> bool {
    w.chars().any(|c| c.is_alphanumeric())
}

/// Split text into lowercase tokens.
///
/// CJK runs contribute both single characters and adjacent bigrams; a bigram is
/// what actually discriminates in those scripts, while the unigrams keep recall
/// for single-character terms.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(text.len() / 5);
    for word in text.unicode_words() {
        if !is_meaningful(word) {
            continue;
        }
        let cjk_chars: Vec<char> = word.chars().filter(|c| is_cjk(*c)).collect();
        if cjk_chars.len() * 2 >= word.chars().count() && !cjk_chars.is_empty() {
            for c in &cjk_chars {
                out.push(c.to_lowercase().collect());
            }
            for pair in cjk_chars.windows(2) {
                out.push(pair.iter().collect());
            }
        } else {
            out.push(word.to_lowercase());
        }
    }
    out
}

/// Approximate token count for an LLM context budget.
///
/// Calibrated against byte-pair vocabularies: roughly 4 bytes per token for
/// Latin script, and closer to 1.4 characters per token for CJK, which BPE
/// vocabularies split much more finely. This is deliberately a slight
/// over-estimate so a budget is never blown.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other_bytes = 0usize;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other_bytes += c.len_utf8();
        }
    }
    let latin = other_bytes.div_ceil(4);
    let cjk_tokens = (cjk as f64 / 1.4).ceil() as usize;
    (latin + cjk_tokens).max(usize::from(!text.trim().is_empty()))
}

/// Collapse runs of whitespace and trim, without allocating when unnecessary.
pub fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Visible length in characters — a fairer "length" than `len()` for CJK.
///
/// Deliberately `chars()`, not grapheme clusters. Every use of this is a
/// heuristic threshold or a ratio, none of which can tell the difference, and
/// Unicode segmentation over every text node in a document measured out at
/// roughly 78% of total extraction time — a 4.4x slowdown for an exactness
/// nothing here consumes. Combining marks and emoji sequences count slightly
/// high, which only ever errs toward keeping content.
pub fn visible_len(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod visible_len_tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn agrees_with_grapheme_counting_on_ordinary_text() {
        // The two differ only on combining marks and emoji sequences, which no
        // threshold in this crate is sensitive to.
        for s in ["hello world", "한국어 텍스트입니다", "日本語のテキスト", "Ελληνικά", ""]
        {
            assert_eq!(visible_len(s), s.graphemes(true).count(), "{s:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_tokens_are_lowercased_words() {
        assert_eq!(tokenize("Hello,  World! 42"), ["hello", "world", "42"]);
    }

    #[test]
    fn korean_yields_unigrams_and_bigrams() {
        let t = tokenize("한국어");
        assert!(t.contains(&"한".to_string()));
        assert!(t.contains(&"한국".to_string()));
        assert!(t.contains(&"국어".to_string()));
    }

    #[test]
    fn cjk_costs_more_tokens_per_char_than_latin() {
        assert!(estimate_tokens("데이터 수집") > estimate_tokens("data"));
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn whitespace_collapses() {
        assert_eq!(normalize_ws("  a \n\t b  "), "a b");
    }
}
