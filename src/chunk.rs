//! Group units into pieces an embedding model will accept.
//!
//! [`Unit`](crate::parse::Unit) is a block: one paragraph, one list item, one
//! heading. That is the right grain for ranking, where a single sentence can
//! be the reason a source was worth reading, and the wrong grain for a vector
//! store. Measured on MDN's `Promise` reference, the median unit is 37 tokens
//! and 36% of them are under 30 -- embed those and you get 125 vectors that
//! each know almost nothing.
//!
//! Chunking walks the units in order and accumulates them until the next one
//! would overrun `target_tokens`, then starts again. A heading begins a new
//! chunk rather than being swallowed by the end of the previous one, so a
//! chunk opens with the thing that says what it is about. `overlap_tokens`
//! repeats whole units from the tail of one chunk at the head of the next,
//! which is what keeps an answer that straddles a boundary retrievable from
//! either side.

use crate::parse::{Article, Unit, UnitKind};

/// A run of units, sized for embedding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Chunk {
    /// Stable within one article: `{position of first unit}`.
    pub index: usize,
    /// Source URL, carried so a retrieved chunk can cite itself.
    pub url: Option<String>,
    /// Document title.
    pub title: Option<String>,
    /// Enclosing headings for the first unit, outermost first.
    pub heading_path: Vec<String>,
    /// Rendered Markdown for the whole run.
    pub markdown: String,
    /// Plain text, which is what you embed.
    pub text: String,
    /// Estimated LLM tokens for [`Chunk::text`].
    pub tokens: usize,
    /// Positions of the units this covers, for tracing back.
    pub units: Vec<usize>,
}

/// How to size chunks.
#[derive(Debug, Clone, Copy)]
pub struct ChunkConfig {
    /// Aim for this many tokens. A single unit longer than this is emitted
    /// whole rather than split -- cutting a paragraph mid-sentence costs more
    /// than an oversized chunk.
    pub target_tokens: usize,
    /// Repeat units from the previous chunk's tail, up to this many tokens.
    pub overlap_tokens: usize,
    /// Drop chunks below this. A stray "Read more" is noise in a vector store.
    pub min_tokens: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        // 512 is what the common sentence-transformer families accept without
        // truncating; 64 of overlap is an eighth, enough to carry a sentence
        // across a boundary without inflating the store by much.
        ChunkConfig { target_tokens: 512, overlap_tokens: 64, min_tokens: 24 }
    }
}

/// Group one article's units into embedding-sized chunks.
pub fn chunk(article: &Article, cfg: &ChunkConfig) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut run: Vec<&Unit> = Vec::new();
    let mut tokens = 0usize;

    for unit in &article.units {
        let starts_section = unit.kind == UnitKind::Heading && !run.is_empty();
        let would_overrun = tokens + unit.tokens > cfg.target_tokens && !run.is_empty();
        if starts_section || would_overrun {
            emit(&mut out, &run, article, cfg);
            run = tail(&run, cfg.overlap_tokens);
            tokens = run.iter().map(|u| u.tokens).sum();
        }
        tokens += unit.tokens;
        run.push(unit);
    }
    emit(&mut out, &run, article, cfg);

    // `min_tokens` is there to drop a trailing "Read more", not to discard a
    // page for being short. An article that produced nothing still gets one
    // chunk: a small source is still a source, and losing it silently is
    // worse than storing a thin vector.
    if out.is_empty() && !article.units.is_empty() {
        let all: Vec<&Unit> = article.units.iter().collect();
        emit(&mut out, &all, article, &ChunkConfig { min_tokens: 0, ..*cfg });
    }
    out
}

/// The trailing units worth repeating in the next chunk.
///
/// Whole units only: half a sentence at the head of a chunk helps nobody.
fn tail<'a>(run: &[&'a Unit], budget: usize) -> Vec<&'a Unit> {
    if budget == 0 {
        return Vec::new();
    }
    let mut kept = Vec::new();
    let mut total = 0;
    for unit in run.iter().rev() {
        if total + unit.tokens > budget {
            break;
        }
        total += unit.tokens;
        kept.push(*unit);
    }
    kept.reverse();
    kept
}

fn emit(out: &mut Vec<Chunk>, run: &[&Unit], article: &Article, cfg: &ChunkConfig) {
    if run.is_empty() {
        return;
    }
    let tokens: usize = run.iter().map(|u| u.tokens).sum();
    if tokens < cfg.min_tokens {
        return;
    }
    let first = run[0];
    out.push(Chunk {
        index: first.position,
        url: article.url.clone(),
        title: article.title().map(str::to_string),
        heading_path: first.heading_path.clone(),
        markdown: run.iter().map(|u| u.markdown.as_str()).collect::<Vec<_>>().join("\n\n"),
        text: run.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join("\n"),
        tokens,
        units: run.iter().map(|u| u.position).collect(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::extract;

    fn article(body: &str) -> Article {
        extract(&format!("<html><body><article>{body}</article></body></html>"), None).unwrap()
    }

    fn long(word: &str, times: usize) -> String {
        format!("<p>{}</p>", format!("{word} ").repeat(times))
    }

    #[test]
    fn small_units_are_grouped_up() {
        let body: String =
            (0..40).map(|i| format!("<p>Sentence number {i} of the body text.</p>")).collect();
        let doc = article(&body);
        let chunks = chunk(&doc, &ChunkConfig::default());
        assert!(chunks.len() < doc.units.len(), "grouping should reduce the count");
        assert!(chunks.iter().all(|c| c.tokens >= 24));
    }

    #[test]
    fn a_heading_opens_a_chunk() {
        let doc =
            article(&format!("{}<h2>Second section</h2>{}", long("alpha", 30), long("omega", 30)));
        let chunks = chunk(&doc, &ChunkConfig { overlap_tokens: 0, ..Default::default() });
        let opener = chunks.iter().find(|c| c.text.contains("Second section"));
        assert!(opener.is_some_and(|c| c.text.starts_with("Second section")));
    }

    #[test]
    fn an_oversized_unit_is_emitted_whole() {
        let doc = article(&long("verylongword", 400));
        let chunks = chunk(&doc, &ChunkConfig { target_tokens: 50, ..Default::default() });
        assert_eq!(chunks.len(), 1, "a paragraph is never cut mid-sentence");
        assert!(chunks[0].tokens > 50);
    }

    #[test]
    fn overlap_repeats_whole_units_and_zero_means_none() {
        let body: String = (0..30)
            .map(|i| format!("<p>Paragraph {i} carrying enough words to count.</p>"))
            .collect();
        let doc = article(&body);
        let with = chunk(
            &doc,
            &ChunkConfig { target_tokens: 60, overlap_tokens: 30, ..Default::default() },
        );
        let without = chunk(
            &doc,
            &ChunkConfig { target_tokens: 60, overlap_tokens: 0, ..Default::default() },
        );
        let sum = |cs: &[Chunk]| cs.iter().map(|c| c.tokens).sum::<usize>();
        assert!(sum(&with) > sum(&without), "overlap repeats text");
        let seen: Vec<usize> = without.iter().flat_map(|c| c.units.clone()).collect();
        let mut sorted = seen.clone();
        sorted.dedup();
        assert_eq!(seen, sorted, "without overlap no unit appears twice");
    }

    #[test]
    fn a_short_article_still_yields_one_chunk() {
        // Dropping it would lose the source, which is worse than a thin vector.
        let doc = article("<p>Short but real, and the only thing on the page.</p>");
        let chunks = chunk(&doc, &ChunkConfig { min_tokens: 500, ..Default::default() });
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn chunks_carry_where_they_came_from() {
        let doc = extract(
            "<html><head><title>T</title></head><body><article><h2>H</h2>\
             <p>Body text long enough to survive the block gate here, and then \
             some more of it so the chunk clears the minimum.</p></article></body></html>",
            Some("https://e.com/p"),
        )
        .unwrap();
        let chunks = chunk(&doc, &ChunkConfig::default());
        assert_eq!(chunks[0].url.as_deref(), Some("https://e.com/p"));
        assert!(chunks[0].title.is_some());
        assert!(!chunks[0].units.is_empty());
    }
}
