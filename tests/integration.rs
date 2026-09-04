//! End-to-end tests over the offline stages.
//!
//! Network behaviour is covered by the Python suite's `--network` tests; what
//! matters here is that parse → denoise → rank compose correctly on documents
//! that look like the real thing.

use rustai_core::parse::{
    ArticleKind, ExtractOptions, IndexMode, extract, extract_many, extract_with,
};
use rustai_core::rank::{SlimConfig, slim};

const NEWS: &str = r##"<!doctype html><html lang="en"><head>
<title>Memory ceilings are a design choice — Example Wire</title>
<meta property="og:site_name" content="Example Wire">
<meta property="article:published_time" content="2026-09-01T09:00:00Z">
</head><body>
<header class="site-header"><nav class="main-nav">
  <a href="/world">World</a><a href="/tech">Tech</a><a href="/business">Business</a>
</nav></header>
<div class="cookie-consent"><p>We use cookies. Accept cookies to continue.</p></div>
<div class="ad-slot"><ins class="adsbygoogle"></ins></div>
<div class="page"><div class="grid"><div class="grid__main">
<article class="article post-content">
  <h1>Memory ceilings are a design choice</h1>
  <div class="byline">By A. Author · <span class="share">Share this</span></div>
  <p>A crawler that allocates a document object model per page inherits that model's memory profile, whatever the language, and the bill arrives in resident set size.</p>
  <h2>What the numbers say</h2>
  <p>Streaming 200 documents through an arena parser held 4.8 MiB resident, against 50.4 MiB for the same work in a garbage-collected runtime.</p>
  <p>Throughput followed: 520 documents per second, or 24.5 megabytes of markup per second, on a single eight-core laptop.</p>
  <ul><li>No per-node allocation in the hot path</li><li>Work stealing across every core</li></ul>
  <pre><code class="language-rust">let doc = Doc::parse(html)?;</code></pre>
  <table><tr><th>Engine</th><th>Peak RSS</th></tr><tr><td>arena</td><td>4.8 MiB</td></tr></table>
  <p>The <a href="/methodology">methodology</a> is published in full, including the corpus generator.</p>
</article></div>
<aside class="sidebar"><div class="widget widget--related"><h3>Related</h3><ul>
  <a href="/r/1">One</a><a href="/r/2">Two</a><a href="/r/3">Three</a>
</ul></div></aside>
</div></div>
<footer class="site-footer"><p>© 2026 Example Wire. All rights reserved. Privacy policy. Terms of service.</p></footer>
<script>window.__DATA__={"tracking":[1,2,3]};</script>
</body></html>"##;

const BLOG: &str = r##"<html><head><title>Notes on arena parsers</title></head><body>
<main><div id="content"><div class="entry-content">
  <h1>Notes on arena parsers</h1>
  <p>An arena parser stores every node in one contiguous buffer, so traversal is a pointer bump rather than a pointer chase across the heap.</p>
  <p>The cost is that you cannot cheaply mutate the tree, which is fine when the only thing you do with it is read it once and throw it away.</p>
</div></div></main>
<div class="comments"><h3>Comments</h3><p>First!</p><p>Nice post, thanks.</p></div>
</body></html>"##;

#[test]
fn extracts_a_news_page_cleanly() {
    let art = extract(NEWS, Some("https://wire.example/story")).unwrap();

    assert_eq!(art.title(), Some("Memory ceilings are a design choice"));
    assert_eq!(art.meta.site_name.as_deref(), Some("Example Wire"));
    assert_eq!(art.meta.published.as_deref(), Some("2026-09-01T09:00:00Z"));

    let text = art.text.to_lowercase();
    for boilerplate in ["accept cookies", "all rights reserved", "privacy policy", "share this"] {
        assert!(!text.contains(boilerplate), "{boilerplate:?} survived:\n{}", art.markdown);
    }
    assert!(!art.markdown.contains("](/world)"), "nav survived");
    assert!(!text.contains("related"), "sidebar survived");

    assert!(art.markdown.contains("# Memory ceilings are a design choice"));
    assert!(art.markdown.contains("## What the numbers say"));
    assert!(art.markdown.contains("- No per-node allocation in the hot path"));
    assert!(art.markdown.contains("```rust\nlet doc = Doc::parse(html)?;\n```"));
    assert!(art.markdown.contains("| Engine | Peak RSS |"));
    assert!(art.markdown.contains("(https://wire.example/methodology)"));

    assert!(art.stats.compression() > 0.5, "only {}", art.stats.compression());
    assert!(art.stats.nodes_dropped > 0);
}

#[test]
fn comment_threads_do_not_become_the_article() {
    let art = extract(BLOG, Some("https://blog.example/arenas")).unwrap();
    assert!(art.text.contains("contiguous buffer"));
    assert!(!art.text.contains("First!"), "comments were selected:\n{}", art.markdown);
}

#[test]
fn ranking_prefers_the_source_that_answers_the_question() {
    let articles = vec![
        extract(NEWS, Some("https://wire.example/story")).unwrap(),
        extract(BLOG, Some("https://blog.example/arenas")).unwrap(),
    ];
    let ctx = slim("how much resident memory did it use", &articles, &SlimConfig::with_budget(180));

    assert!(ctx.markdown.contains("4.8 MiB"), "the answer was not selected:\n{}", ctx.markdown);
    assert!(ctx.tokens <= 180, "budget overrun: {}", ctx.tokens);
    assert!(ctx.markdown.contains("<https://wire.example/story>"), "source not cited");
    assert!(ctx.units_considered >= ctx.selected.len());
}

#[test]
fn budget_is_honoured_across_the_whole_range() {
    let articles = vec![
        extract(NEWS, Some("https://wire.example/story")).unwrap(),
        extract(BLOG, Some("https://blog.example/arenas")).unwrap(),
    ];
    for budget in [0usize, 5, 25, 60, 120, 300, 1000] {
        let ctx = slim("arena parser memory", &articles, &SlimConfig::with_budget(budget));
        assert!(ctx.tokens <= budget, "budget {budget} produced {} tokens", ctx.tokens);
    }
}

#[test]
fn parallel_extraction_matches_serial() {
    let inputs: Vec<(&str, Option<&str>)> =
        vec![(NEWS, Some("https://a.dev/1")), (BLOG, Some("https://b.dev/2"))];
    let parallel = extract_many(inputs.clone(), &ExtractOptions::new());
    assert_eq!(parallel.len(), 2);
    for ((html, url), result) in inputs.iter().zip(parallel) {
        let serial = extract(html, *url).unwrap();
        assert_eq!(result.unwrap().markdown, serial.markdown);
    }
}

const LISTING: &str = r##"<html><body>
<header><nav><a href="/">Home</a><a href="/about">About us</a></nav></header>
<main>
  <h2>Technology</h2>
  <div class="card"><h3><a href="/story/rust">Rust cuts crawler memory by an order of magnitude</a></h3>
    <p>A new pipeline holds under five megabytes while parsing hundreds of documents a second.</p></div>
  <div class="card"><h3><a href="/story/http3">What actually changed in HTTP/3</a></h3>
    <p>The transport moved to UDP, and head-of-line blocking went with it.</p></div>
  <div class="card"><h3><a href="/story/bm25">BM25 is still the baseline to beat</a></h3>
    <p>Twenty years on, the ranking function remains hard to improve upon.</p></div>
  <h2>Opinion</h2>
  <div class="card"><h3><a href="/opinion/agents">Agents are not a product category</a></h3>
    <p>A short argument about naming things properly.</p></div>
  <div class="card"><h3><a href="/opinion/slm">The case for small local models</a></h3>
    <p>Latency, privacy and cost all point in the same direction.</p></div>
  <a href="/page/2">Next</a>
</main>
<footer><a href="/privacy">Privacy policy</a></footer>
</body></html>"##;

#[test]
fn a_listing_page_extracts_as_an_inventory() {
    let art = extract(LISTING, Some("https://wire.example/")).unwrap();

    assert_eq!(art.kind, ArticleKind::Index, "listing was not recognised:\n{}", art.markdown);
    assert_eq!(art.links.len(), 5, "{:#?}", art.links);

    // Stories, with absolute URLs and their standfirsts.
    let bm25 = art.links.iter().find(|l| l.url.ends_with("/story/bm25")).expect("bm25 story");
    assert_eq!(bm25.heading_path, ["Technology"]);
    assert!(bm25.snippet.contains("ranking function"), "{:?}", bm25.snippet);

    // Navigation, pagination and the footer are not inventory.
    for junk in ["/about", "/page/2", "/privacy"] {
        assert!(!art.links.iter().any(|l| l.url.contains(junk)), "{junk} was harvested");
    }

    // And the Markdown is rankable, not a wall of anchors.
    assert!(art.markdown.contains("## Technology"));
    assert!(
        art.markdown
            .contains("- [BM25 is still the baseline to beat](https://wire.example/story/bm25)")
    );
}

#[test]
fn an_article_is_never_mistaken_for_a_listing() {
    for html in [NEWS, BLOG] {
        let art = extract(html, Some("https://example.com/post")).unwrap();
        assert_eq!(art.kind, ArticleKind::Article, "article was read as a listing");
        assert!(art.links.is_empty(), "links were harvested from an article");
    }
}

#[test]
fn index_mode_can_be_forced_or_disabled() {
    let never = ExtractOptions { index_mode: IndexMode::Never, ..ExtractOptions::new() };
    let art = extract_with(LISTING, Some("https://wire.example/"), &never).unwrap();
    assert_eq!(art.kind, ArticleKind::Article);
    assert!(art.links.is_empty());

    // Forcing harvests links from a real article without reclassifying it.
    let always = ExtractOptions { index_mode: IndexMode::Always, ..ExtractOptions::new() };
    let art = extract_with(NEWS, Some("https://wire.example/story"), &always).unwrap();
    assert_eq!(art.kind, ArticleKind::Article);
    assert!(art.markdown.contains("Memory ceilings"), "the article body was lost");
}

#[test]
fn malformed_documents_never_panic() {
    let inputs = [
        "",
        "<ul><li><a href=\"/a\">A link that is long enough to harvest</a></li></ul>",
        "<html>",
        "<div><p>unclosed",
        "<article><p>&amp;&lt;&#x41;&nosuchentity;</p></article>",
        "<article><div><div><div><p>deeply nested but short</p></div></div></div></article>",
        "<article><table><tr><td>a</td></tr>",
    ];
    for html in inputs {
        let art = extract(html, None).expect("extraction must not fail on malformed input");
        let _ = slim("query", std::slice::from_ref(&art), &SlimConfig::default());
    }
}
