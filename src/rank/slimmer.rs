//! The context slimmer.
//!
//! Given a question and a pile of extracted articles, choose the subset of
//! Markdown units that best fills a small model's context window.
//!
//! Selection is greedy MMR — maximal marginal relevance — over a combined score
//! of BM25 relevance, information density and document position, with a
//! redundancy penalty. Redundancy matters more here than in classic retrieval:
//! web results overlap heavily, and three copies of the same paragraph from
//! three aggregators is the single most common way to waste a 4k window.

use rayon::prelude::*;

use crate::parse::{Article, Unit, UnitKind};
use crate::rank::bm25::Bm25;
use crate::rank::density::density_from_tokens;
use crate::text;

/// Markdown level of the per-source header the slimmer writes.
const SOURCE_HEADING_LEVEL: usize = 2;

/// Tokens reserved for the blank line and any breadcrumb around a unit.
///
/// The budget has to cover what the slimmer itself writes — source headers,
/// URLs, breadcrumbs, elision markers — not just the units. Reserving here
/// keeps the estimate close; [`fit_to_budget`] then guarantees it.
const UNIT_OVERHEAD_TOKENS: usize = 4;

/// Tuning for [`slim`].
#[derive(Debug, Clone)]
pub struct SlimConfig {
    /// Hard budget for the produced context, in estimated LLM tokens.
    pub max_tokens: usize,
    /// Weight of BM25 relevance in the combined score.
    pub relevance_weight: f32,
    /// Weight of information density.
    pub density_weight: f32,
    /// Weight of the "earlier in the document" prior.
    pub position_weight: f32,
    /// How hard to penalise similarity to already-selected units, in `[0, 1]`.
    pub diversity: f32,
    /// Units scoring below this are never selected.
    pub min_score: f32,
    /// Information-density floor. A block below it is dropped even when the
    /// budget has room — an unfilled window beats a window full of filler.
    pub min_density: f32,
    /// Drop a unit that overlaps an already-selected one by more than this.
    pub dedupe_threshold: f32,
    /// Cap on tokens taken from any one source, so a single long page cannot
    /// crowd out corroborating sources.
    pub max_tokens_per_source: Option<usize>,
    /// Prefix each run of units with its heading breadcrumb.
    pub include_breadcrumbs: bool,
    /// Emit a `[…]` marker where units were skipped.
    pub mark_elisions: bool,
}

impl Default for SlimConfig {
    fn default() -> Self {
        SlimConfig {
            max_tokens: 2048,
            relevance_weight: 0.60,
            density_weight: 0.25,
            position_weight: 0.15,
            diversity: 0.35,
            min_score: 0.02,
            min_density: 0.10,
            dedupe_threshold: 0.80,
            max_tokens_per_source: None,
            include_breadcrumbs: true,
            mark_elisions: true,
        }
    }
}

impl SlimConfig {
    /// A config for a given context budget, leaving everything else default.
    pub fn with_budget(max_tokens: usize) -> Self {
        SlimConfig { max_tokens, ..Default::default() }
    }
}

/// One unit that made it into the context, with its scoring breakdown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Selected {
    /// Index into the articles passed to [`slim`].
    pub source: usize,
    /// Index into that article's `units`.
    pub unit: usize,
    /// Combined score.
    pub score: f32,
    /// Normalised BM25 relevance in `[0, 1]`.
    pub relevance: f32,
    /// Information density in `[0, 1]`.
    pub density: f32,
    /// Estimated tokens contributed.
    pub tokens: usize,
}

/// A source that contributed at least one unit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceRef {
    /// Index into the articles passed to [`slim`].
    pub index: usize,
    /// Page URL, if known.
    pub url: Option<String>,
    /// Page title, if known.
    pub title: Option<String>,
    /// Tokens taken from this source.
    pub tokens: usize,
}

/// The finished context.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Context {
    /// The query it was built for.
    pub query: String,
    /// Ready-to-prompt Markdown.
    pub markdown: String,
    /// Estimated tokens in [`Context::markdown`].
    pub tokens: usize,
    /// What was selected, in output order.
    pub selected: Vec<Selected>,
    /// Sources that contributed, in output order.
    pub sources: Vec<SourceRef>,
    /// Units that were available before selection.
    pub units_considered: usize,
}

struct Candidate<'a> {
    source: usize,
    unit_idx: usize,
    unit: &'a Unit,
    tokens: Vec<String>,
    base: f32,
    relevance: f32,
    density: f32,
    boilerplate: u32,
}

/// Build a context for `query` out of `articles`, under a token budget.
///
/// An empty query is legal and means "summarise": relevance drops out and the
/// selection is driven by density and position alone.
pub fn slim(query: &str, articles: &[Article], cfg: &SlimConfig) -> Context {
    let query_tokens = text::tokenize(query);

    // Flatten to a unit corpus. Headings are kept even when short because they
    // are what makes an excerpt navigable.
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for (si, article) in articles.iter().enumerate() {
        for (ui, unit) in article.units.iter().enumerate() {
            let long_enough = unit.kind == UnitKind::Heading
                || text::visible_len(&unit.text) >= 24
                || unit.kind == UnitKind::Code;
            if long_enough && !unit.markdown.is_empty() {
                candidates.push(Candidate {
                    source: si,
                    unit_idx: ui,
                    unit,
                    tokens: Vec::new(),
                    base: 0.0,
                    relevance: 0.0,
                    density: 0.0,
                    boilerplate: 0,
                });
            }
        }
    }
    let units_considered = candidates.len();
    if candidates.is_empty() {
        return Context {
            query: query.to_string(),
            markdown: String::new(),
            tokens: 0,
            selected: Vec::new(),
            sources: Vec::new(),
            units_considered: 0,
        };
    }

    // Tokenisation and density are independent per unit and dominate the cost
    // of scoring, so they run across the rayon pool.
    let scored: Vec<(Vec<String>, f32, u32)> = candidates
        .par_iter()
        .map(|c| {
            let toks = text::tokenize(&c.unit.text);
            let d = density_from_tokens(&c.unit.text, &toks);
            (toks, d.score, d.boilerplate_hits)
        })
        .collect();
    for (c, (toks, d, bp)) in candidates.iter_mut().zip(scored) {
        c.tokens = toks;
        c.density = d;
        c.boilerplate = bp;
    }

    let corpus: Vec<Vec<String>> = candidates.iter().map(|c| c.tokens.clone()).collect();
    let index = Bm25::from_tokens(&corpus);
    let raw_relevance = if query_tokens.is_empty() {
        vec![0.0; candidates.len()]
    } else {
        index.score_all(&query_tokens)
    };
    let max_rel = raw_relevance.iter().copied().fold(0.0f32, f32::max);

    // With no usable query signal, fold relevance's weight into density so the
    // budget still goes to the most informative blocks.
    let (w_rel, w_den) = if max_rel > 0.0 {
        (cfg.relevance_weight, cfg.density_weight)
    } else {
        (0.0, cfg.density_weight + cfg.relevance_weight)
    };

    let source_lengths: Vec<usize> = articles.iter().map(|a| a.units.len().max(1)).collect();
    for (c, raw) in candidates.iter_mut().zip(&raw_relevance) {
        c.relevance = if max_rel > 0.0 { raw / max_rel } else { 0.0 };
        // Journalism and documentation both front-load: a decaying prior on
        // document position is a weak signal, but a consistently correct one.
        let rel_pos = c.unit_idx as f32 / source_lengths[c.source] as f32;
        let position = 1.0 - rel_pos * 0.7;
        c.base = (w_rel * c.relevance + w_den * c.density + cfg.position_weight * position)
            * c.unit.kind.prior();
    }

    // --- greedy MMR -------------------------------------------------------
    let mut chosen: Vec<usize> = Vec::new();
    let mut taken = vec![false; candidates.len()];
    let mut used_tokens = 0usize;
    let mut per_source = vec![0usize; articles.len()];
    let mut source_seen = vec![false; articles.len()];
    let source_overhead: Vec<usize> = articles
        .iter()
        .map(|a| {
            let title = a.title().unwrap_or("Untitled");
            let url = a.url.as_deref().unwrap_or("");
            text::estimate_tokens(&format!("## {title}\n<{url}>")) + 2
        })
        .collect();

    loop {
        let mut best: Option<(usize, f32)> = None;
        for (i, c) in candidates.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let scaffolding = UNIT_OVERHEAD_TOKENS
                + if source_seen[c.source] { 0 } else { source_overhead[c.source] };
            if used_tokens + c.unit.tokens + scaffolding > cfg.max_tokens {
                continue;
            }
            // Two distinct legal/CTA phrases in one block is a precise enough
            // signal to reject outright rather than merely score down.
            if c.boilerplate >= 2 {
                taken[i] = true;
                continue;
            }
            if c.unit.kind != UnitKind::Heading && c.density < cfg.min_density {
                taken[i] = true;
                continue;
            }
            if let Some(cap) = cfg.max_tokens_per_source
                && per_source[c.source] + c.unit.tokens > cap
            {
                continue;
            }
            let max_sim = chosen
                .iter()
                .map(|&j| overlap(&c.tokens, &candidates[j].tokens))
                .fold(0.0f32, f32::max);
            if max_sim >= cfg.dedupe_threshold {
                taken[i] = true; // near-duplicate: never reconsider it
                continue;
            }
            let mmr = c.base - cfg.diversity * max_sim;
            if mmr < cfg.min_score {
                continue;
            }
            if best.is_none_or(|(_, b)| mmr > b) {
                best = Some((i, mmr));
            }
        }
        match best {
            Some((i, _)) => {
                let source = candidates[i].source;
                taken[i] = true;
                used_tokens += candidates[i].unit.tokens + UNIT_OVERHEAD_TOKENS;
                if !source_seen[source] {
                    source_seen[source] = true;
                    used_tokens += source_overhead[source];
                }
                per_source[source] += candidates[i].unit.tokens;
                chosen.push(i);
            }
            None => break,
        }
    }

    // --- render -----------------------------------------------------------
    // Selection reserved an estimate for the scaffolding; this makes the budget
    // a guarantee. Rendering is cheap next to fetching, so we render, measure
    // the real cost, drop the weakest unit and try again until it fits.
    chosen.sort_by_key(|&i| (candidates[i].source, candidates[i].unit_idx));
    let (markdown, selected, sources) = fit_to_budget(&mut chosen, &candidates, articles, cfg);

    let tokens = text::estimate_tokens(&markdown);
    Context { query: query.to_string(), markdown, tokens, selected, sources, units_considered }
}

/// Render, and if the result overruns the budget, drop the lowest-scoring unit
/// and render again.
fn fit_to_budget(
    chosen: &mut Vec<usize>,
    candidates: &[Candidate<'_>],
    articles: &[Article],
    cfg: &SlimConfig,
) -> (String, Vec<Selected>, Vec<SourceRef>) {
    loop {
        let rendered = render(chosen, candidates, articles, cfg);
        if chosen.is_empty() || text::estimate_tokens(&rendered.0) <= cfg.max_tokens {
            return rendered;
        }
        let weakest = chosen
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| candidates[**a].base.total_cmp(&candidates[**b].base))
            .map(|(pos, _)| pos)
            .expect("chosen is non-empty");
        chosen.remove(weakest);
    }
}

/// Write the selected units as Markdown, with source headers and breadcrumbs.
fn render(
    chosen: &[usize],
    candidates: &[Candidate<'_>],
    articles: &[Article],
    cfg: &SlimConfig,
) -> (String, Vec<Selected>, Vec<SourceRef>) {
    let mut markdown = String::new();
    let mut selected = Vec::with_capacity(chosen.len());
    let mut sources: Vec<SourceRef> = Vec::new();
    let mut last_source: Option<usize> = None;
    let mut last_unit_idx: Option<usize> = None;
    let mut last_path: Vec<String> = Vec::new();

    for &i in chosen {
        let c = &candidates[i];
        let record = Selected {
            source: c.source,
            unit: c.unit_idx,
            score: c.base,
            relevance: c.relevance,
            density: c.density,
            tokens: c.unit.tokens,
        };

        if last_source != Some(c.source) {
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            let article = &articles[c.source];
            let title = article.title().unwrap_or("Untitled").to_string();
            let hashes = "#".repeat(SOURCE_HEADING_LEVEL);
            match &article.url {
                Some(url) => markdown.push_str(&format!("{hashes} {title}\n<{url}>")),
                None => markdown.push_str(&format!("{hashes} {title}")),
            }
            sources.push(SourceRef {
                index: c.source,
                url: article.url.clone(),
                title: article.title().map(str::to_string),
                tokens: 0,
            });
            last_source = Some(c.source);
            last_unit_idx = None;
            // The `## title` line above already establishes this level, so a
            // breadcrumb repeating it would be pure token waste.
            last_path = vec![title];
        }

        // Likewise for the document's own `<h1>`: it is the title we just wrote.
        let is_title_echo = c.unit.kind == UnitKind::Heading
            && c.unit.heading_path.is_empty()
            && Some(c.unit.text.as_str()) == articles[c.source].title();
        if is_title_echo {
            last_unit_idx = Some(c.unit_idx);
            if let Some(s) = sources.last_mut() {
                s.tokens += c.unit.tokens;
            }
            selected.push(record);
            continue;
        }

        if cfg.mark_elisions
            && let Some(prev) = last_unit_idx
            && c.unit_idx > prev + 1
        {
            markdown.push_str("\n\n[…]");
        }

        if cfg.include_breadcrumbs
            && c.unit.kind != UnitKind::Heading
            && !c.unit.heading_path.is_empty()
            && c.unit.heading_path != last_path
        {
            markdown.push_str(&format!("\n\n**{}**", c.unit.heading_path.join(" › ")));
            last_path = c.unit.heading_path.clone();
        }

        markdown.push_str("\n\n");
        if c.unit.kind == UnitKind::Heading {
            // The source header occupies `##`, so a document's own headings are
            // demoted to sit beneath it. Left alone, an article's `<h1>` would
            // render as a sibling of the source it came from.
            let level = (c.unit.level as usize + SOURCE_HEADING_LEVEL).min(6);
            markdown.push_str(&format!("{} {}", "#".repeat(level), c.unit.text));
            last_path = c.unit.heading_path.clone();
            last_path.push(c.unit.text.clone());
        } else {
            markdown.push_str(&c.unit.markdown);
        }
        last_unit_idx = Some(c.unit_idx);

        if let Some(s) = sources.last_mut() {
            s.tokens += c.unit.tokens;
        }
        selected.push(record);
    }

    (markdown.trim().to_string(), selected, sources)
}

/// Overlap coefficient: `|A ∩ B| / min(|A|, |B|)`.
///
/// Chosen over Jaccard because a short paragraph fully contained in a longer
/// one scores 1.0 here and only ~0.4 under Jaccard — and containment is exactly
/// what syndicated copy looks like.
fn overlap(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let large: std::collections::HashSet<&str> = large.iter().map(String::as_str).collect();
    let small_set: std::collections::HashSet<&str> = small.iter().map(String::as_str).collect();
    let hits = small_set.iter().filter(|t| large.contains(**t)).count();
    hits as f32 / small_set.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::extract;

    fn article(url: &str, body: &str) -> Article {
        extract(body, Some(url)).unwrap()
    }

    const A: &str = r#"<article><h1>Tokio and rayon</h1>
      <p>Tokio drives the asynchronous I/O: thousands of in-flight requests on a handful of OS threads, with no per-request stack.</p>
      <p>Rayon then handles the CPU side, parsing every fetched document across a work-stealing pool of exactly as many threads as there are cores.</p>
      <p>Subscribe to our newsletter for more. All rights reserved. Privacy policy. Follow us on social media today.</p>
      </article>"#;

    const B: &str = r#"<article><h1>Cooking with paprika</h1>
      <p>Paprika is best bloomed in fat before the liquid goes in, which keeps the flavour from turning dusty and flat.</p>
      <p>Smoked varieties come from peppers dried over oak, and they will overwhelm a delicate stock if you are not careful.</p>
      </article>"#;

    #[test]
    fn on_topic_source_dominates_the_budget() {
        let arts = vec![article("https://a.dev/1", A), article("https://b.dev/2", B)];
        let ctx = slim("tokio rayon parallel parsing", &arts, &SlimConfig::with_budget(200));
        assert!(!ctx.markdown.is_empty());
        let from_a: usize = ctx.selected.iter().filter(|s| s.source == 0).map(|s| s.tokens).sum();
        let from_b: usize = ctx.selected.iter().filter(|s| s.source == 1).map(|s| s.tokens).sum();
        assert!(from_a > from_b, "a={from_a} b={from_b}\n{}", ctx.markdown);
    }

    #[test]
    fn respects_the_token_budget() {
        let arts = vec![article("https://a.dev/1", A), article("https://b.dev/2", B)];
        // The budget must cover the rendered output — source headers, URLs and
        // breadcrumbs included — not just the units that were selected.
        for budget in [20usize, 30, 50, 80, 150, 400] {
            let ctx = slim("tokio rayon", &arts, &SlimConfig::with_budget(budget));
            assert!(
                ctx.tokens <= budget,
                "budget {budget} overrun: rendered {} tokens\n{}",
                ctx.tokens,
                ctx.markdown
            );
        }
    }

    #[test]
    fn near_duplicate_sources_are_not_both_kept() {
        let arts = vec![article("https://a.dev/1", A), article("https://mirror.dev/1", A)];
        let ctx = slim("tokio rayon", &arts, &SlimConfig::with_budget(400));
        assert_eq!(ctx.sources.len(), 1, "mirror survived:\n{}", ctx.markdown);
    }

    #[test]
    fn boilerplate_paragraph_is_not_selected() {
        let arts = vec![article("https://a.dev/1", A)];
        let ctx = slim("tokio rayon", &arts, &SlimConfig::with_budget(400));
        assert!(
            !ctx.markdown.contains("All rights reserved"),
            "boilerplate selected:\n{}",
            ctx.markdown
        );
    }

    #[test]
    fn headings_are_demoted_below_the_source_header() {
        let arts = vec![article("https://a.dev/1", A)];
        let ctx = slim("tokio rayon parallel", &arts, &SlimConfig::with_budget(400));
        assert!(ctx.markdown.starts_with("## Tokio and rayon"));
        // The article's own `<h1>` must not become a sibling of the source header.
        for line in ctx.markdown.lines().skip(1) {
            assert!(
                !line.starts_with("# ") && !line.starts_with("## "),
                "heading collided with the source header: {line:?}\n{}",
                ctx.markdown
            );
        }
    }

    #[test]
    fn output_cites_its_sources() {
        let arts = vec![article("https://a.dev/1", A)];
        let ctx = slim("tokio", &arts, &SlimConfig::with_budget(400));
        assert!(ctx.markdown.contains("<https://a.dev/1>"));
        assert!(ctx.markdown.contains("## Tokio and rayon"));
        assert_eq!(ctx.sources[0].url.as_deref(), Some("https://a.dev/1"));
    }

    #[test]
    fn empty_query_falls_back_to_density() {
        let arts = vec![article("https://a.dev/1", A)];
        let ctx = slim("", &arts, &SlimConfig::with_budget(200));
        assert!(!ctx.selected.is_empty());
        assert!(ctx.selected.iter().all(|s| s.relevance == 0.0));
    }

    #[test]
    fn no_articles_yields_empty_context() {
        let ctx = slim("anything", &[], &SlimConfig::default());
        assert_eq!(ctx.tokens, 0);
        assert_eq!(ctx.units_considered, 0);
    }

    #[test]
    fn overlap_detects_containment() {
        let a = text::tokenize("tokio drives the asynchronous io");
        let b = text::tokenize("tokio drives the asynchronous io on a handful of threads");
        assert!(overlap(&a, &b) > 0.95);
        assert!(overlap(&a, &text::tokenize("paprika and oak")) < 0.2);
    }
}
