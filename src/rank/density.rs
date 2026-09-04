//! Information density.
//!
//! BM25 answers "does this block match the query". It cannot tell a dense
//! factual paragraph from a hedge like *"In this article we will explore some
//! of the things you need to know."* — which matches query terms perfectly and
//! carries no information at all. Density is the second axis, and it is what
//! keeps a small model's context window full of facts instead of throat-clearing.
//!
//! Four components, all cheap and all language-agnostic enough to survive
//! Korean and English in the same corpus:
//!
//! * **content-word ratio** — tokens left after stopwords and Korean particles
//! * **numeric ratio** — figures, dates, versions, measurements
//! * **type-token ratio** — vocabulary spread, which collapses on filler
//! * **boilerplate penalty** — an explicit blocklist of legal and CTA phrasing

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::text;

/// High-frequency function words with no topical content.
const STOPWORDS_EN: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "be", "been",
    "but", "by", "can", "come", "could", "do", "does", "for", "from", "get", "go", "had", "has",
    "have", "he", "her", "here", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its",
    "just", "like", "make", "may", "me", "more", "most", "my", "no", "not", "now", "of", "on",
    "one", "only", "or", "other", "our", "out", "over", "print", "said", "same", "see", "she",
    "should", "so", "some", "such", "take", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "those", "through", "to", "too", "up", "us", "use", "very",
    "was", "we", "well", "were", "what", "when", "where", "which", "while", "who", "why", "will",
    "with", "would", "you", "your",
];

/// Korean particles and endings that survive whitespace tokenisation as
/// standalone tokens, plus the most common function words.
const STOPWORDS_KO: &[&str] = &[
    "그",
    "그리고",
    "그러나",
    "그런",
    "그런데",
    "그리하여",
    "및",
    "또는",
    "또한",
    "이",
    "저",
    "것",
    "수",
    "등",
    "때",
    "안",
    "위",
    "중",
    "약",
    "더",
    "잘",
    "다시",
    "하지만",
    "때문",
    "통해",
    "대한",
    "대해",
    "있다",
    "없다",
    "하다",
    "한다",
    "된다",
    "이다",
];

static STOPWORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOPWORDS_EN.iter().chain(STOPWORDS_KO.iter()).copied().collect());

/// Phrases that mark a block as page furniture rather than content.
const BOILERPLATE: &[&str] = &[
    "all rights reserved",
    "terms of service",
    "privacy policy",
    "cookie policy",
    "we use cookies",
    "accept cookies",
    "sign up for",
    "subscribe to our",
    "newsletter",
    "follow us on",
    "share this",
    "read more",
    "click here",
    "advertisement",
    "관련 기사",
    "무단 전재",
    "재배포 금지",
    "저작권자",
    "구독하기",
    "댓글",
    "광고",
];

/// Density breakdown for one block of text.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Density {
    /// Fraction of tokens that are not stopwords.
    pub content_ratio: f32,
    /// Fraction of tokens that carry a digit.
    pub numeric_ratio: f32,
    /// Unique tokens over total tokens.
    pub type_token_ratio: f32,
    /// Number of boilerplate phrases matched.
    pub boilerplate_hits: u32,
    /// Combined score in `[0, 1]`.
    pub score: f32,
}

impl Density {
    /// A neutral score, for empty input.
    pub const ZERO: Density = Density {
        content_ratio: 0.0,
        numeric_ratio: 0.0,
        type_token_ratio: 0.0,
        boilerplate_hits: 0,
        score: 0.0,
    };
}

/// Score a block of plain text.
pub fn density(text_in: &str) -> Density {
    let tokens = text::tokenize(text_in);
    density_from_tokens(text_in, &tokens)
}

/// Score with a token list you already have, to avoid re-tokenising.
pub fn density_from_tokens(raw: &str, tokens: &[String]) -> Density {
    if tokens.is_empty() {
        return Density::ZERO;
    }
    let total = tokens.len() as f32;

    let content = tokens.iter().filter(|t| !STOPWORDS.contains(t.as_str())).count() as f32;
    let numeric = tokens.iter().filter(|t| t.chars().any(|c| c.is_ascii_digit())).count() as f32;
    let unique: HashSet<&str> = tokens.iter().map(String::as_str).collect();

    let content_ratio = content / total;
    let numeric_ratio = numeric / total;
    let type_token_ratio = unique.len() as f32 / total;

    let lower = raw.to_lowercase();
    let boilerplate_hits = BOILERPLATE.iter().filter(|p| lower.contains(**p)).count() as u32;

    // Numeric content is worth a bounded bonus, not a linear one — a table of
    // nothing but figures is not four times as informative as a paragraph with
    // one date in it.
    let numeric_bonus = (numeric_ratio * 3.0).min(1.0);

    // Very short blocks cannot be judged reliably, so damp their score toward
    // the middle rather than letting a two-word fragment score 1.0.
    let length_conf = (total / 12.0).min(1.0);

    let raw_score = 0.45 * content_ratio + 0.30 * type_token_ratio + 0.25 * numeric_bonus;
    let penalty = 1.0 - (boilerplate_hits as f32 * 0.35).min(0.9);
    let score = (raw_score * penalty * (0.4 + 0.6 * length_conf)).clamp(0.0, 1.0);

    Density { content_ratio, numeric_ratio, type_token_ratio, boilerplate_hits, score }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_beat_filler() {
        let facts = density(
            "The 0.1.0 release cut resident memory from 412 MB to 28 MB and parsed 1,200 documents in 3.4 seconds.",
        );
        let filler = density(
            "In this article we are going to take a look at some of the things that you will want to know about it.",
        );
        assert!(facts.score > filler.score, "facts {facts:?} filler {filler:?}");
        assert!(facts.numeric_ratio > 0.2);
    }

    #[test]
    fn boilerplate_is_penalised() {
        let plain = density("The parser walks the arena once and caches three metrics per node.");
        let legal = density(
            "The parser walks the arena once and caches three metrics per node. All rights reserved. Privacy policy.",
        );
        assert!(legal.boilerplate_hits >= 2);
        assert!(legal.score < plain.score, "legal {legal:?} plain {plain:?}");
    }

    #[test]
    fn korean_filler_scores_below_korean_facts() {
        let facts = density("2026년 기준 메모리 점유율은 28MB, 처리 속도는 초당 1200건이다.");
        let filler = density(
            "이 글에서는 그것에 대해 조금 더 알아보려고 합니다. 그리고 또한 그런 것들도 있습니다.",
        );
        assert!(facts.score > filler.score, "facts {facts:?} filler {filler:?}");
    }

    #[test]
    fn empty_text_is_zero() {
        assert_eq!(density("").score, 0.0);
        assert_eq!(density("   ").score, 0.0);
    }

    #[test]
    fn very_short_blocks_are_damped() {
        let short = density("Rust fast");
        let long = density(
            "Rust is fast because the arena parser avoids per-node allocation and the pool avoids copies.",
        );
        assert!(short.score < long.score, "short {short:?} long {long:?}");
    }
}
