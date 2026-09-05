//! The collection router.
//!
//! Free sources only, queried concurrently, then fused. No provider here needs
//! an API key or a credit card, which is the entire point: the cost of a
//! research query should be bandwidth, not a per-call fee.
//!
//! Individual providers are flaky by nature — a public SearXNG instance may be
//! down, DuckDuckGo may rate-limit — so the router treats a provider failure as
//! a partial result rather than an error, and reports what failed alongside
//! what worked.

mod academic;
mod community;
mod duckduckgo;
mod feeds;
mod searxng;
mod wikipedia;

use std::collections::HashMap;

use url::Url;

use crate::error::Result;
use crate::http::Fetcher;

pub use feeds::{parse_rss, parse_sitemap};

/// Reciprocal-rank-fusion constant. 60 is the value from the original paper and
/// is not worth tuning: it only sets how fast rank influence decays.
const RRF_K: f32 = 60.0;

/// Query parameters that identify a campaign, not a document.
const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "fbclid",
    "gclid",
    "dclid",
    "msclkid",
    "mc_cid",
    "mc_eid",
    "igshid",
    "ref",
    "ref_src",
    "spm",
    "yclid",
    "_ga",
    "s_kwcid",
];

/// Where results can come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// DuckDuckGo's HTML endpoint.
    DuckDuckGo,
    /// arXiv's Atom API — preprints in physics, maths, CS and related fields.
    Arxiv,
    /// OpenAlex, the open index of scholarly works across every discipline.
    OpenAlex,
    /// Crossref, the DOI registry's metadata index.
    Crossref,
    /// Europe PMC — the life-sciences literature, including PubMed, PMC and
    /// preprints, in a single request.
    EuropePmc,
    /// Hacker News, through the Algolia index behind its own search.
    HackerNews,
    /// Stack Exchange search over one site, by site key (`stackoverflow` by
    /// default).
    StackExchange(String),
    /// GitHub repository search.
    GitHub,
    /// The MediaWiki search API for a given language code.
    Wikipedia(String),
    /// A SearXNG instance, by base URL.
    SearxNG(String),
    /// An RSS or Atom feed, by URL.
    Rss(String),
    /// A `sitemap.xml` or sitemap index, by URL.
    Sitemap(String),
    /// An HTML listing page — a front page, archive or feed rendered for
    /// people — harvested for the links it offers.
    Index(String),
}

impl Provider {
    /// Short stable name, as it appears on [`SearchResult::provider`].
    pub fn name(&self) -> &'static str {
        match self {
            Provider::DuckDuckGo => "duckduckgo",
            Provider::Arxiv => "arxiv",
            Provider::OpenAlex => "openalex",
            Provider::Crossref => "crossref",
            Provider::EuropePmc => "europepmc",
            Provider::HackerNews => "hackernews",
            Provider::StackExchange(_) => "stackexchange",
            Provider::GitHub => "github",
            Provider::Wikipedia(_) => "wikipedia",
            Provider::SearxNG(_) => "searxng",
            Provider::Rss(_) => "rss",
            Provider::Sitemap(_) => "sitemap",
            Provider::Index(_) => "index",
        }
    }

    /// Parse the string form used by the Python API.
    ///
    /// `"duckduckgo"`, `"wikipedia"`, `"wikipedia:ko"`, `"searxng:https://…"`,
    /// `"rss:https://…"`, `"sitemap:https://…"`.
    pub fn parse(spec: &str) -> Result<Provider> {
        let spec = spec.trim();
        let (kind, arg) = match spec.split_once(':') {
            Some((k, a)) if !a.starts_with("//") => (k, a.trim()),
            _ => (spec, ""),
        };
        Ok(match kind.to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Provider::DuckDuckGo,
            "arxiv" => Provider::Arxiv,
            "openalex" => Provider::OpenAlex,
            "crossref" => Provider::Crossref,
            "europepmc" | "pubmed" | "pmc" => Provider::EuropePmc,
            "hackernews" | "hn" => Provider::HackerNews,
            "stackexchange" | "stackoverflow" | "se" => {
                Provider::StackExchange(if arg.is_empty() {
                    "stackoverflow".into()
                } else {
                    arg.into()
                })
            }
            "github" | "gh" => Provider::GitHub,
            "wikipedia" | "wiki" => {
                Provider::Wikipedia(if arg.is_empty() { "en".into() } else { arg.into() })
            }
            "searxng" | "searx" => {
                if arg.is_empty() {
                    return Err(crate::Error::Config(
                        "searxng needs an instance URL, e.g. `searxng:https://searx.be`".into(),
                    ));
                }
                Provider::SearxNG(arg.into())
            }
            "rss" | "feed" | "atom" => Provider::Rss(require_url(arg, "rss")?),
            "sitemap" => Provider::Sitemap(require_url(arg, "sitemap")?),
            "index" | "page" | "listing" => Provider::Index(require_url(arg, "index")?),
            other => {
                return Err(crate::Error::Config(format!("unknown provider `{other}`")));
            }
        })
    }
}

fn require_url(arg: &str, kind: &str) -> Result<String> {
    if arg.is_empty() {
        return Err(crate::Error::Config(format!("{kind} needs a URL")));
    }
    Ok(arg.to_string())
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchResult {
    /// Result title.
    pub title: String,
    /// Absolute URL.
    pub url: String,
    /// Provider-supplied snippet, plain text.
    pub snippet: String,
    /// Providers that returned this URL.
    pub providers: Vec<String>,
    /// Fused score. Higher is better; only meaningful relative to siblings.
    pub score: f32,
    /// Best rank this URL achieved at any single provider, zero-based.
    pub rank: usize,
}

/// Router settings.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Providers to query, in no particular order.
    pub providers: Vec<Provider>,
    /// Maximum results to return after fusion.
    pub limit: usize,
    /// Maximum results to request from each provider.
    pub per_provider: usize,
    /// Contact address for the OpenAlex and Crossref "polite pools".
    ///
    /// Both APIs route identified callers to a faster, more reliable pool.
    /// Leaving this unset works, but is slower and more likely to be throttled.
    pub contact_email: Option<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            providers: vec![Provider::DuckDuckGo, Provider::Wikipedia("en".into())],
            limit: 10,
            per_provider: 15,
            contact_email: None,
        }
    }
}

/// What a search returned, including what went wrong.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchOutcome {
    /// Fused, deduplicated results, best first.
    pub results: Vec<SearchResult>,
    /// `(provider, message)` for each provider that failed.
    pub failures: Vec<(String, String)>,
}

/// Runs providers concurrently and fuses their rankings.
#[derive(Debug, Clone)]
pub struct Router {
    fetcher: Fetcher,
    cfg: SearchConfig,
}

impl Router {
    /// Build a router over an existing fetcher.
    pub fn new(fetcher: Fetcher, cfg: SearchConfig) -> Self {
        Router { fetcher, cfg }
    }

    /// The fetcher this router uses, for reusing its connection pool.
    pub fn fetcher(&self) -> &Fetcher {
        &self.fetcher
    }

    /// Run the query against every configured provider.
    pub async fn search(&self, query: &str) -> SearchOutcome {
        let limit = self.cfg.per_provider;
        let contact = self.cfg.contact_email.clone();
        let tasks = self.cfg.providers.iter().map(|p| {
            let fetcher = self.fetcher.clone();
            let provider = p.clone();
            let query = query.to_string();
            let contact = contact.clone();
            async move {
                let out = match &provider {
                    Provider::DuckDuckGo => duckduckgo::search(&fetcher, &query, limit).await,
                    Provider::Arxiv => academic::arxiv(&fetcher, &query, limit).await,
                    Provider::OpenAlex => {
                        academic::openalex(&fetcher, &query, limit, contact.as_deref()).await
                    }
                    Provider::Crossref => {
                        academic::crossref(&fetcher, &query, limit, contact.as_deref()).await
                    }
                    Provider::EuropePmc => academic::europe_pmc(&fetcher, &query, limit).await,
                    Provider::HackerNews => community::hacker_news(&fetcher, &query, limit).await,
                    Provider::StackExchange(site) => {
                        community::stack_exchange(&fetcher, site, &query, limit).await
                    }
                    Provider::GitHub => community::github(&fetcher, &query, limit).await,
                    Provider::Wikipedia(lang) => {
                        wikipedia::search(&fetcher, &query, lang, limit).await
                    }
                    Provider::SearxNG(base) => searxng::search(&fetcher, base, &query, limit).await,
                    Provider::Rss(url) => feeds::search_rss(&fetcher, url, &query, limit).await,
                    Provider::Sitemap(url) => {
                        feeds::search_sitemap(&fetcher, url, &query, limit).await
                    }
                    Provider::Index(url) => feeds::search_index(&fetcher, url, &query, limit).await,
                };
                (provider, out)
            }
        });

        let outcomes = futures_util::future::join_all(tasks).await;
        let mut failures = Vec::new();
        let mut ranked: Vec<(&'static str, Vec<RawHit>)> = Vec::new();
        for (provider, out) in &outcomes {
            match out {
                Ok(hits) => ranked.push((provider.name(), hits.clone())),
                Err(e) => failures.push((provider.name().to_string(), e.to_string())),
            }
        }
        SearchOutcome { results: fuse(&ranked, self.cfg.limit), failures }
    }
}

/// A provider's raw output, before fusion.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawHit {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
}

/// Reciprocal rank fusion.
///
/// Chosen over score averaging because provider scores are not comparable —
/// DuckDuckGo has no score at all, Wikipedia's is a relevance number on its own
/// scale. Ranks are the only signal every provider actually shares.
fn fuse(ranked: &[(&'static str, Vec<RawHit>)], limit: usize) -> Vec<SearchResult> {
    struct Acc {
        title: String,
        url: String,
        snippet: String,
        providers: Vec<String>,
        score: f32,
        best_rank: usize,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (provider, hits) in ranked {
        for (rank, hit) in hits.iter().enumerate() {
            let Some(key) = canonical_key(&hit.url) else { continue };
            let contribution = 1.0 / (RRF_K + rank as f32);
            match acc.get_mut(&key) {
                Some(existing) => {
                    existing.score += contribution;
                    existing.best_rank = existing.best_rank.min(rank);
                    if !existing.providers.iter().any(|p| p == provider) {
                        existing.providers.push(provider.to_string());
                    }
                    // Prefer the longest snippet: providers truncate differently
                    // and a longer one is strictly more useful downstream.
                    if hit.snippet.len() > existing.snippet.len() {
                        existing.snippet = hit.snippet.clone();
                    }
                    if existing.title.is_empty() {
                        existing.title = hit.title.clone();
                    }
                }
                None => {
                    order.push(key.clone());
                    acc.insert(
                        key,
                        Acc {
                            title: hit.title.clone(),
                            url: hit.url.clone(),
                            snippet: hit.snippet.clone(),
                            providers: vec![provider.to_string()],
                            score: contribution,
                            best_rank: rank,
                        },
                    );
                }
            }
        }
    }

    let mut out: Vec<SearchResult> = order
        .into_iter()
        .filter_map(|k| acc.remove(&k))
        .map(|a| SearchResult {
            title: a.title,
            url: a.url,
            snippet: a.snippet,
            providers: a.providers,
            score: a.score,
            rank: a.best_rank,
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.rank.cmp(&b.rank)));
    out.truncate(limit);
    out
}

/// A stable identity for a URL, for deduplication across providers.
///
/// Scheme, `www.`, tracking parameters, fragments and a trailing slash are all
/// noise: the same article arrives from three providers with three different
/// decorations, and counting it once is what makes fusion mean anything.
pub fn canonical_key(url: &str) -> Option<String> {
    let mut parsed = Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);

    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        let mut sorted = kept;
        sorted.sort();
        let mut q = parsed.query_pairs_mut();
        q.clear();
        for (k, v) in &sorted {
            q.append_pair(k, v);
        }
        drop(q);
    }

    let host = parsed.host_str()?.trim_start_matches("www.").to_ascii_lowercase();
    let path = parsed.path().trim_end_matches('/');
    let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!("{host}{path}{query}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str) -> RawHit {
        RawHit { title: "T".into(), url: url.into(), snippet: "s".into() }
    }

    #[test]
    fn academic_provider_specs_parse() {
        assert_eq!(Provider::parse("arxiv").unwrap(), Provider::Arxiv);
        assert_eq!(Provider::parse("OpenAlex").unwrap(), Provider::OpenAlex);
        assert_eq!(Provider::parse("crossref").unwrap(), Provider::Crossref);
        assert_eq!(Provider::parse("arxiv").unwrap().name(), "arxiv");
    }

    #[test]
    fn provider_specs_parse() {
        assert_eq!(Provider::parse("ddg").unwrap(), Provider::DuckDuckGo);
        assert_eq!(Provider::parse("wikipedia:ko").unwrap(), Provider::Wikipedia("ko".into()));
        assert_eq!(Provider::parse("wiki").unwrap(), Provider::Wikipedia("en".into()));
        assert_eq!(
            Provider::parse("searxng:https://searx.be").unwrap(),
            Provider::SearxNG("https://searx.be".into())
        );
        assert_eq!(
            Provider::parse("index:https://news.example/").unwrap(),
            Provider::Index("https://news.example/".into())
        );
        assert!(Provider::parse("searxng").is_err());
        assert!(Provider::parse("index").is_err());
        assert!(Provider::parse("tavily").is_err());
    }

    #[test]
    fn canonicalisation_collapses_decorations() {
        let a = canonical_key("https://www.Example.com/post/?utm_source=x&fbclid=y#top");
        let b = canonical_key("http://example.com/post");
        assert_eq!(a, b, "{a:?} vs {b:?}");
    }

    #[test]
    fn canonicalisation_keeps_meaningful_query() {
        let a = canonical_key("https://example.com/s?b=2&a=1").unwrap();
        let b = canonical_key("https://example.com/s?a=1&b=2").unwrap();
        assert_eq!(a, b);
        assert!(a.contains("a=1"));
        assert_ne!(a, canonical_key("https://example.com/s?a=9").unwrap());
    }

    #[test]
    fn agreement_between_providers_wins() {
        let ranked = vec![
            ("duckduckgo", vec![hit("https://a.dev/1"), hit("https://b.dev/2")]),
            ("wikipedia", vec![hit("https://c.dev/3"), hit("https://b.dev/2?utm_source=q")]),
        ];
        let fused = fuse(&ranked, 10);
        assert_eq!(fused[0].url, "https://b.dev/2", "fused: {fused:#?}");
        assert_eq!(fused[0].providers, ["duckduckgo", "wikipedia"]);
        assert_eq!(fused.len(), 3, "the duplicate should have collapsed");
    }

    #[test]
    fn limit_is_respected_and_bad_urls_dropped() {
        let ranked = vec![(
            "duckduckgo",
            vec![hit("https://a.dev/1"), hit("javascript:void"), hit("https://b.dev/2")],
        )];
        let fused = fuse(&ranked, 1);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].url, "https://a.dev/1");
    }
}
