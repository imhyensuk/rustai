//! The end-to-end pipeline: a question in, a context window out.
//!
//! Each stage is usable on its own — [`Pipeline::search`], [`Pipeline::read`],
//! [`crate::parse::extract`], [`crate::rank::slim`] — but [`Pipeline::research`]
//! is the reason the crate exists. It runs the network stages on tokio and the
//! CPU stages on rayon, which is the split that keeps the memory ceiling flat:
//! no stage ever holds more than the documents currently in flight.

use rayon::prelude::*;

use crate::error::Result;
use crate::http::{FetchConfig, Fetcher, Page};
use crate::parse::{Article, ExtractOptions, extract_with};
use crate::rank::{Context, SlimConfig, slim};
use crate::search::{Router, SearchConfig, SearchOutcome, SearchResult};

/// A configured pipeline: fetcher, providers, extractor and slimmer.
#[derive(Debug, Clone)]
pub struct Pipeline {
    router: Router,
    extract: ExtractOptions,
    slim: SlimConfig,
}

/// Everything one research run produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Research {
    /// The query as asked.
    pub query: String,
    /// The context to hand to a model.
    pub context: Context,
    /// Search hits, best first, including ones that failed to fetch.
    pub results: Vec<SearchResult>,
    /// Successfully extracted articles, in the order they were read.
    pub articles: Vec<Article>,
    /// `(stage, message)` for anything that went wrong without stopping the run.
    pub failures: Vec<(String, String)>,
}

impl Pipeline {
    /// Build a pipeline with default settings everywhere.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    /// Start configuring a pipeline.
    pub fn builder() -> PipelineBuilder {
        PipelineBuilder::default()
    }

    /// The underlying fetcher, for reusing its connection pool.
    pub fn fetcher(&self) -> &Fetcher {
        self.router.fetcher()
    }

    /// Run a search across the configured providers.
    pub async fn search(&self, query: &str) -> SearchOutcome {
        self.router.search(query).await
    }

    /// Fetch URLs concurrently. Order matches the input.
    pub async fn fetch(&self, urls: &[String]) -> Vec<Result<Page>> {
        self.fetcher().fetch_many(urls).await
    }

    /// Fetch and extract, dropping nothing: failures come back in place.
    pub async fn read(&self, urls: &[String]) -> Vec<Result<Article>> {
        let pages = self.fetch(urls).await;
        // Fetching was I/O-bound and is done; extraction is pure CPU, so it
        // moves to rayon rather than blocking a tokio worker.
        pages
            .into_par_iter()
            .map(|page| {
                let page = page?;
                extract_with(&page.body, Some(&page.final_url), &self.extract)
            })
            .collect()
    }

    /// Search, read the top results, and compress them into a context window.
    ///
    /// `max_sources` caps how many result pages are actually fetched, which is
    /// the only knob that meaningfully changes how long a run takes.
    pub async fn research(&self, query: &str, max_sources: usize) -> Research {
        let outcome = self.search(query).await;
        let mut failures: Vec<(String, String)> =
            outcome.failures.iter().map(|(p, m)| (format!("search:{p}"), m.clone())).collect();

        let urls: Vec<String> =
            outcome.results.iter().take(max_sources).map(|r| r.url.clone()).collect();
        let read = self.read(&urls).await;

        let mut articles = Vec::with_capacity(read.len());
        for (url, result) in urls.iter().zip(read) {
            match result {
                // A page that yielded nothing usable is a failure, not an
                // empty source: keeping it would only dilute the ranking.
                Ok(article) if !article.units.is_empty() => articles.push(article),
                Ok(_) => failures.push((format!("extract:{url}"), "no content extracted".into())),
                Err(e) => failures.push((format!("fetch:{url}"), e.to_string())),
            }
        }

        let context = slim(query, &articles, &self.slim);
        Research { query: query.to_string(), context, results: outcome.results, articles, failures }
    }
}

/// Builder for [`Pipeline`].
#[derive(Debug, Clone, Default)]
pub struct PipelineBuilder {
    fetch: FetchConfig,
    search: SearchConfig,
    extract: ExtractOptions,
    slim: SlimConfig,
}

impl PipelineBuilder {
    /// Replace the fetch configuration.
    pub fn fetch(mut self, cfg: FetchConfig) -> Self {
        self.fetch = cfg;
        self
    }

    /// Replace the search configuration.
    pub fn search(mut self, cfg: SearchConfig) -> Self {
        self.search = cfg;
        self
    }

    /// Replace the extraction configuration.
    pub fn extract(mut self, cfg: ExtractOptions) -> Self {
        self.extract = cfg;
        self
    }

    /// Replace the slimming configuration.
    pub fn slim(mut self, cfg: SlimConfig) -> Self {
        self.slim = cfg;
        self
    }

    /// Build the pipeline.
    pub fn build(self) -> Result<Pipeline> {
        let fetcher = Fetcher::with_config(self.fetch)?;
        Ok(Pipeline {
            router: Router::new(fetcher, self.search),
            extract: self.extract,
            slim: self.slim,
        })
    }
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Provider;

    #[test]
    fn builder_wires_every_stage() {
        let pipeline = Pipeline::builder()
            .fetch(FetchConfig { concurrency: 4, ..Default::default() })
            .search(SearchConfig {
                providers: vec![Provider::Wikipedia("ko".into())],
                limit: 3,
                per_provider: 5,
            })
            .slim(SlimConfig::with_budget(512))
            .build()
            .unwrap();
        assert_eq!(pipeline.fetcher().config().concurrency, 4);
        assert_eq!(pipeline.slim.max_tokens, 512);
    }

    #[test]
    fn default_pipeline_builds() {
        assert!(Pipeline::new().is_ok());
    }

    #[test]
    fn invalid_fetch_config_fails_the_build() {
        let err =
            Pipeline::builder().fetch(FetchConfig { concurrency: 0, ..Default::default() }).build();
        assert!(err.is_err());
    }
}
