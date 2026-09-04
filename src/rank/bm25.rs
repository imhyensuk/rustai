//! Okapi BM25 over extracted units.
//!
//! A "document" here is one Markdown unit, not one web page. Ranking at unit
//! granularity is the whole point: the answer to a question is usually two
//! paragraphs of a page, and a page-level ranker cannot tell you which two.

use std::collections::HashMap;

use crate::text;

/// Term-saturation parameter. 1.2 is the standard default.
pub const DEFAULT_K1: f32 = 1.2;
/// Length-normalisation parameter. 0.75 is the standard default.
pub const DEFAULT_B: f32 = 0.75;

/// A built BM25 index.
#[derive(Debug, Clone)]
pub struct Bm25 {
    k1: f32,
    b: f32,
    avgdl: f32,
    /// Per-document term frequencies.
    tf: Vec<HashMap<String, u32>>,
    /// Per-document length in tokens.
    len: Vec<u32>,
    /// Document frequency per term.
    df: HashMap<String, u32>,
}

impl Bm25 {
    /// Index a corpus of already-tokenised documents.
    pub fn from_tokens(docs: &[Vec<String>]) -> Self {
        Self::from_tokens_with(docs, DEFAULT_K1, DEFAULT_B)
    }

    /// Index with explicit parameters.
    pub fn from_tokens_with(docs: &[Vec<String>], k1: f32, b: f32) -> Self {
        let mut tf: Vec<HashMap<String, u32>> = Vec::with_capacity(docs.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        let mut len = Vec::with_capacity(docs.len());
        let mut total = 0u64;

        for doc in docs {
            let mut counts: HashMap<String, u32> = HashMap::new();
            for t in doc {
                *counts.entry(t.clone()).or_insert(0) += 1;
            }
            for term in counts.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
            total += doc.len() as u64;
            len.push(doc.len() as u32);
            tf.push(counts);
        }

        let avgdl = if docs.is_empty() { 0.0 } else { total as f32 / docs.len() as f32 };
        Bm25 { k1, b, avgdl: avgdl.max(1.0), tf, len, df }
    }

    /// Index a corpus of raw strings, tokenising with [`crate::text::tokenize`].
    pub fn from_texts<S: AsRef<str>>(docs: &[S]) -> Self {
        let toks: Vec<Vec<String>> = docs.iter().map(|d| text::tokenize(d.as_ref())).collect();
        Self::from_tokens(&toks)
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.tf.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.tf.is_empty()
    }

    /// Robertson/Sparck-Jones IDF with the `+1` smoothing that keeps very
    /// common terms at a small positive weight instead of a negative one.
    fn idf(&self, term: &str) -> f32 {
        let n = self.tf.len() as f32;
        let df = self.df.get(term).copied().unwrap_or(0) as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// BM25 score of document `idx` against a tokenised query.
    pub fn score(&self, query: &[String], idx: usize) -> f32 {
        let Some(tf) = self.tf.get(idx) else { return 0.0 };
        let dl = self.len[idx] as f32;
        let norm = 1.0 - self.b + self.b * (dl / self.avgdl);
        let mut total = 0.0;
        for term in query {
            let f = tf.get(term).copied().unwrap_or(0) as f32;
            if f == 0.0 {
                continue;
            }
            total += self.idf(term) * (f * (self.k1 + 1.0)) / (f + self.k1 * norm);
        }
        total
    }

    /// Score every document, in index order.
    pub fn score_all(&self, query: &[String]) -> Vec<f32> {
        (0..self.tf.len()).map(|i| self.score(query, i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<&'static str> {
        vec![
            "rust async runtime tokio handles concurrent io",
            "python crawlers use a lot of memory and are slow",
            "tokio and rayon combine async io with data parallelism",
            "the weather today is mild and pleasant",
        ]
    }

    #[test]
    fn ranks_the_on_topic_document_first() {
        let idx = Bm25::from_texts(&corpus());
        let q = text::tokenize("tokio rayon parallelism");
        let scores = idx.score_all(&q);
        let best =
            scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap();
        assert_eq!(best, 2, "scores were {scores:?}");
        assert_eq!(scores[3], 0.0, "off-topic doc should score zero");
    }

    #[test]
    fn rare_terms_outweigh_common_ones() {
        let idx = Bm25::from_texts(&corpus());
        assert!(idx.idf("rayon") > idx.idf("and"));
    }

    #[test]
    fn works_on_korean_text() {
        let docs = ["러스트 비동기 런타임", "파이썬 크롤러 메모리", "비동기 병렬 처리"];
        let idx = Bm25::from_texts(&docs);
        let scores = idx.score_all(&text::tokenize("비동기 런타임"));
        assert!(scores[0] > scores[1], "{scores:?}");
    }

    #[test]
    fn empty_corpus_is_safe() {
        let idx = Bm25::from_texts::<&str>(&[]);
        assert!(idx.is_empty());
        assert!(idx.score_all(&text::tokenize("anything")).is_empty());
    }
}
