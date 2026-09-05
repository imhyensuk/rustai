//! Python bindings.
//!
//! The shape of this API is deliberately boring: blocking methods that look
//! like `requests`, returning plain objects with attributes. Async Rust is an
//! implementation detail, not something a caller should have to adopt.
//!
//! Every method that touches the network or the rayon pool releases the GIL for
//! its whole duration, so a `rustai` call in one Python thread does not stall
//! the others — which is most of the point of doing this work outside Python.

use std::sync::OnceLock;

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::denoise::{DenoiseConfig, DenoiseStats};
use crate::error::Error;
use crate::http::{FetchConfig, Impersonate, Page};
use crate::parse::{
    Article, ArticleKind, ExtractOptions, IndexMode, Link, Meta, RenderOptions, Unit,
};
use crate::pipeline::{Pipeline, Research};
use crate::rank::{Context, SlimConfig};
use crate::search::{Provider, SearchConfig, SearchResult};

create_exception!(rustai, RustaiError, PyException, "Base class for every rustai error.");
create_exception!(rustai, NetworkError, RustaiError, "The request failed at the transport level.");
create_exception!(
    rustai,
    HttpStatusError,
    RustaiError,
    "The server answered with a failure status."
);
create_exception!(rustai, RobotsError, RustaiError, "robots.txt disallows this URL.");
create_exception!(rustai, ExtractError, RustaiError, "The document could not be parsed.");
create_exception!(rustai, ProviderError, RustaiError, "A search provider failed.");
create_exception!(rustai, BrowserError, RustaiError, "The headless fallback was unavailable.");

impl From<Error> for PyErr {
    fn from(e: Error) -> PyErr {
        let msg = e.to_string();
        match e {
            Error::Network { .. } => NetworkError::new_err(msg),
            Error::Status { .. } => HttpStatusError::new_err(msg),
            Error::BodyTooLarge { .. } => NetworkError::new_err(msg),
            Error::RobotsDenied(_) => RobotsError::new_err(msg),
            Error::InvalidUrl(_) | Error::Config(_) => PyValueError::new_err(msg),
            Error::Parse(_) => ExtractError::new_err(msg),
            Error::Provider { .. } => ProviderError::new_err(msg),
            Error::Browser(_) => BrowserError::new_err(msg),
        }
    }
}

/// One multi-threaded tokio runtime for the process.
///
/// Building a runtime per call would cost more than most of the calls do, and
/// a shared one lets the connection pool actually be a pool.
fn runtime() -> PyResult<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build())
        .as_ref()
        .map_err(|e| RustaiError::new_err(format!("could not start the async runtime: {e}")))
}

/// Convert serialised data into plain Python containers.
fn json_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    use serde_json::Value;
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.into_pyobject(py)?.into_any(),
            (_, Some(f)) => f.into_pyobject(py)?.into_any(),
            _ => py.None().into_bound(py),
        },
        Value::String(s) => s.into_pyobject(py)?.into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, val) in map {
                dict.set_item(k, json_to_py(py, val)?)?;
            }
            dict.into_any()
        }
    })
}

fn to_dict<T: serde::Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    let json = serde_json::to_value(value)
        .map_err(|e| RustaiError::new_err(format!("could not serialise: {e}")))?;
    Ok(json_to_py(py, &json)?.unbind())
}

// ---------------------------------------------------------------- data types

/// Document metadata read from `<head>`.
#[pyclass(name = "Meta", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyMeta {
    inner: Meta,
}

#[pymethods]
impl PyMeta {
    /// Best available title.
    #[getter]
    fn title(&self) -> Option<&str> {
        self.inner.title.as_deref()
    }
    /// Meta or Open Graph description.
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }
    /// Declared author.
    #[getter]
    fn byline(&self) -> Option<&str> {
        self.inner.byline.as_deref()
    }
    /// Publication timestamp, verbatim from the page.
    #[getter]
    fn published(&self) -> Option<&str> {
        self.inner.published.as_deref()
    }
    /// BCP-47 language tag.
    #[getter]
    fn language(&self) -> Option<&str> {
        self.inner.language.as_deref()
    }
    /// Canonical URL.
    #[getter]
    fn canonical(&self) -> Option<&str> {
        self.inner.canonical.as_deref()
    }
    /// Site name.
    #[getter]
    fn site_name(&self) -> Option<&str> {
        self.inner.site_name.as_deref()
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("Meta(title={:?}, language={:?})", self.inner.title, self.inner.language)
    }
}

/// One block of an extracted document.
#[pyclass(name = "Unit", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyUnit {
    inner: Unit,
}

#[pymethods]
impl PyUnit {
    /// `"heading"`, `"paragraph"`, `"list_item"`, `"code"`, `"quote"` or `"table"`.
    #[getter]
    fn kind(&self) -> String {
        serde_json::to_value(self.inner.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }
    /// Heading level, or list nesting depth.
    #[getter]
    fn level(&self) -> u8 {
        self.inner.level
    }
    /// Plain text.
    #[getter]
    fn text(&self) -> &str {
        &self.inner.text
    }
    /// Rendered Markdown.
    #[getter]
    fn markdown(&self) -> &str {
        &self.inner.markdown
    }
    /// Enclosing headings, outermost first.
    #[getter]
    fn heading_path(&self) -> Vec<String> {
        self.inner.heading_path.clone()
    }
    /// Index in document order.
    #[getter]
    fn position(&self) -> usize {
        self.inner.position
    }
    /// Estimated tokens.
    #[getter]
    fn tokens(&self) -> usize {
        self.inner.tokens
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        let preview: String = self.inner.text.chars().take(40).collect();
        format!("Unit(kind={}, tokens={}, text={preview:?})", self.kind(), self.inner.tokens)
    }
}

/// What the denoiser removed.
#[pyclass(name = "DenoiseStats", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyStats {
    inner: DenoiseStats,
}

#[pymethods]
impl PyStats {
    /// Nodes considered.
    #[getter]
    fn nodes_visited(&self) -> usize {
        self.inner.nodes_visited
    }
    /// Nodes rejected as boilerplate.
    #[getter]
    fn nodes_dropped(&self) -> usize {
        self.inner.nodes_dropped
    }
    /// Bytes of HTML in.
    #[getter]
    fn html_bytes(&self) -> usize {
        self.inner.html_bytes
    }
    /// Bytes of Markdown out.
    #[getter]
    fn markdown_bytes(&self) -> usize {
        self.inner.markdown_bytes
    }
    /// Size reduction in `[0, 1]`.
    #[getter]
    fn compression(&self) -> f32 {
        self.inner.compression()
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "DenoiseStats(html_bytes={}, markdown_bytes={}, compression={:.2})",
            self.inner.html_bytes,
            self.inner.markdown_bytes,
            self.inner.compression()
        )
    }
}

/// One link harvested from a listing page.
#[pyclass(name = "Link", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyLink {
    inner: Link,
}

#[pymethods]
impl PyLink {
    /// Anchor text.
    #[getter]
    fn text(&self) -> &str {
        &self.inner.text
    }
    /// Absolute URL.
    #[getter]
    fn url(&self) -> &str {
        &self.inner.url
    }
    /// Nearby descriptive text, if the page offered any.
    #[getter]
    fn snippet(&self) -> &str {
        &self.inner.snippet
    }
    /// Enclosing section headings, outermost first.
    #[getter]
    fn heading_path(&self) -> Vec<String> {
        self.inner.heading_path.clone()
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!("Link(text={:?}, url={:?})", self.inner.text, self.inner.url)
    }
}

/// A cleaned document.
#[pyclass(name = "Article", frozen, module = "rustai", from_py_object)]
#[derive(Clone)]
pub struct PyArticle {
    pub(crate) inner: Article,
}

#[pymethods]
impl PyArticle {
    /// Source URL.
    #[getter]
    fn url(&self) -> Option<&str> {
        self.inner.url.as_deref()
    }
    /// Title, falling back to the first heading.
    #[getter]
    fn title(&self) -> Option<&str> {
        self.inner.title()
    }
    /// The whole cleaned document as Markdown.
    #[getter]
    fn markdown(&self) -> &str {
        &self.inner.markdown
    }
    /// The whole cleaned document as plain text.
    #[getter]
    fn text(&self) -> &str {
        &self.inner.text
    }
    /// `"article"` for prose, `"index"` for a listing page.
    #[getter]
    fn kind(&self) -> &'static str {
        match self.inner.kind {
            ArticleKind::Article => "article",
            ArticleKind::Index => "index",
        }
    }
    /// Links harvested from a listing page.
    #[getter]
    fn links(&self) -> Vec<PyLink> {
        self.inner.links.iter().cloned().map(|inner| PyLink { inner }).collect()
    }
    /// Metadata from `<head>`.
    #[getter]
    fn meta(&self) -> PyMeta {
        PyMeta { inner: self.inner.meta.clone() }
    }
    /// Rankable blocks.
    #[getter]
    fn units(&self) -> Vec<PyUnit> {
        self.inner.units.iter().cloned().map(|inner| PyUnit { inner }).collect()
    }
    /// Denoiser statistics.
    #[getter]
    fn stats(&self) -> PyStats {
        PyStats { inner: self.inner.stats.clone() }
    }
    /// Estimated tokens for the whole article.
    #[getter]
    fn tokens(&self) -> usize {
        self.inner.tokens()
    }
    /// The article as nested dicts.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __len__(&self) -> usize {
        self.inner.units.len()
    }
    fn __repr__(&self) -> String {
        format!(
            "Article(kind={}, title={:?}, units={}, tokens={})",
            self.kind(),
            self.inner.title(),
            self.inner.units.len(),
            self.inner.tokens()
        )
    }
}

/// A raw HTTP response.
#[pyclass(name = "Page", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyPage {
    inner: Page,
}

#[pymethods]
impl PyPage {
    /// URL as requested.
    #[getter]
    fn url(&self) -> &str {
        &self.inner.url
    }
    /// URL after redirects.
    #[getter]
    fn final_url(&self) -> &str {
        &self.inner.final_url
    }
    /// HTTP status code.
    #[getter]
    fn status(&self) -> u16 {
        self.inner.status
    }
    /// `Content-Type` header.
    #[getter]
    fn content_type(&self) -> Option<&str> {
        self.inner.content_type.as_deref()
    }
    /// Decoded response body.
    #[getter]
    fn body(&self) -> &str {
        &self.inner.body
    }
    /// Wall time in milliseconds.
    #[getter]
    fn elapsed_ms(&self) -> u64 {
        self.inner.elapsed_ms
    }
    /// Whether headless Chrome produced this body.
    #[getter]
    fn rendered(&self) -> bool {
        self.inner.rendered
    }
    /// Extract this page without fetching it again.
    fn extract(&self) -> PyResult<PyArticle> {
        let inner = crate::parse::extract(&self.inner.body, Some(&self.inner.final_url))?;
        Ok(PyArticle { inner })
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "Page(url={:?}, status={}, bytes={}, elapsed_ms={})",
            self.inner.url,
            self.inner.status,
            self.inner.body.len(),
            self.inner.elapsed_ms
        )
    }
}

/// One search hit.
#[pyclass(name = "SearchResult", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PySearchResult {
    inner: SearchResult,
}

#[pymethods]
impl PySearchResult {
    /// Result title.
    #[getter]
    fn title(&self) -> &str {
        &self.inner.title
    }
    /// Absolute URL.
    #[getter]
    fn url(&self) -> &str {
        &self.inner.url
    }
    /// Provider snippet.
    #[getter]
    fn snippet(&self) -> &str {
        &self.inner.snippet
    }
    /// Providers that returned this URL.
    #[getter]
    fn providers(&self) -> Vec<String> {
        self.inner.providers.clone()
    }
    /// Fused rank-fusion score.
    #[getter]
    fn score(&self) -> f32 {
        self.inner.score
    }
    /// Best position this URL reached at any *single* provider, zero-based.
    ///
    /// Not this result's position in the fused list — for that, enumerate the
    /// list you were handed. A result ranked first by two providers and a
    /// result ranked first by one both report `0`; what separates them is
    /// `score`, which is what the list is sorted by.
    #[getter]
    fn rank(&self) -> usize {
        self.inner.rank
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __repr__(&self) -> String {
        format!(
            "SearchResult(title={:?}, url={:?}, providers={:?})",
            self.inner.title, self.inner.url, self.inner.providers
        )
    }
}

/// A compressed context window.
#[pyclass(name = "Context", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyContext {
    inner: Context,
}

#[pymethods]
impl PyContext {
    /// The query it was built for.
    #[getter]
    fn query(&self) -> &str {
        &self.inner.query
    }
    /// Ready-to-prompt Markdown.
    #[getter]
    fn markdown(&self) -> &str {
        &self.inner.markdown
    }
    /// Estimated tokens.
    #[getter]
    fn tokens(&self) -> usize {
        self.inner.tokens
    }
    /// Sources that contributed, as dicts.
    #[getter]
    fn sources(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner.sources)
    }
    /// Why each unit was chosen, as dicts.
    ///
    /// Keys: `source` and `unit`, which are indices rather than text --
    /// `articles[source].units[unit]` is the block itself -- plus `tokens`
    /// and the three scores behind the decision: `relevance` (BM25 against
    /// the query), `density` (nouns and numbers over filler) and the combined
    /// `score`. Enough to answer "why is this in my context window and that
    /// paragraph is not".
    #[getter]
    fn selected(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner.selected)
    }
    /// Units available before selection.
    #[getter]
    fn units_considered(&self) -> usize {
        self.inner.units_considered
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __str__(&self) -> &str {
        &self.inner.markdown
    }
    fn __repr__(&self) -> String {
        format!(
            "Context(tokens={}, sources={}, selected={}/{})",
            self.inner.tokens,
            self.inner.sources.len(),
            self.inner.selected.len(),
            self.inner.units_considered
        )
    }
}

/// The result of a full research run.
#[pyclass(name = "Research", frozen, module = "rustai", skip_from_py_object)]
#[derive(Clone)]
pub struct PyResearch {
    inner: Research,
}

#[pymethods]
impl PyResearch {
    /// The query as asked.
    #[getter]
    fn query(&self) -> &str {
        &self.inner.query
    }
    /// The compressed context.
    #[getter]
    fn context(&self) -> PyContext {
        PyContext { inner: self.inner.context.clone() }
    }
    /// Ready-to-prompt Markdown, straight from the context.
    #[getter]
    fn markdown(&self) -> &str {
        &self.inner.context.markdown
    }
    /// Search hits, best first.
    #[getter]
    fn results(&self) -> Vec<PySearchResult> {
        self.inner.results.iter().cloned().map(|inner| PySearchResult { inner }).collect()
    }
    /// Articles that were successfully read.
    #[getter]
    fn articles(&self) -> Vec<PyArticle> {
        self.inner.articles.iter().cloned().map(|inner| PyArticle { inner }).collect()
    }
    /// `(stage, message)` pairs for non-fatal failures.
    #[getter]
    fn failures(&self) -> Vec<(String, String)> {
        self.inner.failures.clone()
    }
    /// Everything above, as a dict.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        to_dict(py, &self.inner)
    }
    fn __str__(&self) -> &str {
        &self.inner.context.markdown
    }
    fn __repr__(&self) -> String {
        format!(
            "Research(query={:?}, articles={}, tokens={}, failures={})",
            self.inner.query,
            self.inner.articles.len(),
            self.inner.context.tokens,
            self.inner.failures.len()
        )
    }
}

// -------------------------------------------------------------------- client

/// A reusable pipeline: connection pool, providers, extractor and slimmer.
#[pyclass(name = "Client", frozen, module = "rustai")]
pub struct PyClient {
    pipeline: Pipeline,
    summary: String,
}

#[pymethods]
impl PyClient {
    /// Build a client.
    ///
    /// `providers` accepts `"duckduckgo"`, `"wikipedia"` or `"wikipedia:ko"`,
    /// `"searxng:https://…"`, `"rss:https://…"` and `"sitemap:https://…"`.
    #[new]
    #[pyo3(signature = (
        *,
        providers = None,
        concurrency = 16,
        timeout = 20.0,
        impersonate = "chrome",
        respect_robots = true,
        per_host_delay = 0.25,
        retries = 2,
        max_body_bytes = 8 * 1024 * 1024,
        accept_language = "en-US,en;q=0.9",
        browser_fallback = false,
        contact_email = None,
        proxies = None,
        max_retry_after = 60.0,
        cookie_file = None,
        max_tokens = 2048,
        diversity = 0.35,
        include_links = true,
        include_images = false,
        include_tables = true,
        index_mode = "auto",
        limit = 10,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        providers: Option<Vec<String>>,
        concurrency: usize,
        timeout: f64,
        impersonate: &str,
        respect_robots: bool,
        per_host_delay: f64,
        retries: u32,
        max_body_bytes: usize,
        accept_language: &str,
        browser_fallback: bool,
        contact_email: Option<String>,
        proxies: Option<Vec<String>>,
        max_retry_after: f64,
        cookie_file: Option<String>,
        max_tokens: usize,
        diversity: f32,
        include_links: bool,
        include_images: bool,
        include_tables: bool,
        index_mode: &str,
        limit: usize,
    ) -> PyResult<Self> {
        if !timeout.is_finite() || timeout <= 0.0 {
            return Err(PyValueError::new_err("timeout must be a positive number of seconds"));
        }
        if !(0.0..=1.0).contains(&diversity) {
            return Err(PyValueError::new_err("diversity must be between 0.0 and 1.0"));
        }

        let providers = match providers {
            Some(specs) => {
                specs.iter().map(|s| Provider::parse(s)).collect::<Result<Vec<_>, _>>()?
            }
            None => SearchConfig::default().providers,
        };
        if providers.is_empty() {
            return Err(PyValueError::new_err("at least one provider is required"));
        }

        let fetch = FetchConfig {
            concurrency,
            timeout: std::time::Duration::from_secs_f64(timeout),
            max_body_bytes,
            retries,
            per_host_delay: std::time::Duration::from_secs_f64(per_host_delay.max(0.0)),
            respect_robots,
            impersonate: Impersonate::parse(impersonate)?,
            accept_language: accept_language.to_string(),
            browser_fallback,
            proxies: proxies.unwrap_or_default(),
            max_retry_after: std::time::Duration::from_secs_f64(max_retry_after.max(0.0)),
            cookie_file: cookie_file.map(std::path::PathBuf::from),
            ..Default::default()
        };
        let summary = format!(
            "Client(providers={:?}, concurrency={concurrency}, impersonate={impersonate:?}, \
             respect_robots={respect_robots}, proxies={}, max_tokens={max_tokens})",
            providers.iter().map(Provider::name).collect::<Vec<_>>(),
            fetch.proxies.len()
        );

        let pipeline = Pipeline::builder()
            .fetch(fetch)
            .search(SearchConfig { providers, limit, per_provider: limit.max(10), contact_email })
            .extract(ExtractOptions {
                render: RenderOptions { include_links, include_images, include_tables },
                denoise: DenoiseConfig { keep_tables: include_tables, ..Default::default() },
                index_mode: parse_index_mode(index_mode)?,
                ..ExtractOptions::new()
            })
            .slim(SlimConfig { max_tokens, diversity, ..Default::default() })
            .build()?;

        Ok(PyClient { pipeline, summary })
    }

    /// Search every configured provider and return fused results.
    ///
    /// Provider failures are not raised: a partial result set is more useful
    /// than an exception when one of five providers is having a bad day. Pass
    /// `strict=True` to raise when *every* provider failed.
    #[pyo3(signature = (query, *, strict = false))]
    fn search(&self, py: Python<'_>, query: &str, strict: bool) -> PyResult<Vec<PySearchResult>> {
        let outcome = py.detach(|| runtime().map(|rt| rt.block_on(self.pipeline.search(query))))?;
        if strict && outcome.results.is_empty() && !outcome.failures.is_empty() {
            let detail = outcome
                .failures
                .iter()
                .map(|(p, m)| format!("{p}: {m}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(ProviderError::new_err(format!("every provider failed ({detail})")));
        }
        Ok(outcome.results.into_iter().map(|inner| PySearchResult { inner }).collect())
    }

    /// Fetch URLs concurrently.
    ///
    /// **The result is not 1:1 with the input.** With `raise_on_error=False`
    /// (the default) a failed URL is simply absent, so one dead link cannot
    /// lose you the other nineteen — but neither can you tell which one went,
    /// and indexing the result against the input you passed will be wrong.
    /// Match on `Page.url` instead, or pass `raise_on_error=True` when a
    /// missing page is a problem rather than a nuisance.
    ///
    /// This differs deliberately from `extract_many`, which is 1:1 and raises:
    /// there, an input is a document you already hold, and losing one silently
    /// would be a bug in your own pipeline. Here an input is somebody else's
    /// server, and it is allowed to be down.
    #[pyo3(signature = (urls, *, raise_on_error = false))]
    fn fetch(
        &self,
        py: Python<'_>,
        urls: Vec<String>,
        raise_on_error: bool,
    ) -> PyResult<Vec<PyPage>> {
        let results = py.detach(|| runtime().map(|rt| rt.block_on(self.pipeline.fetch(&urls))))?;
        collect(results, raise_on_error)
            .map(|pages| pages.into_iter().map(|inner| PyPage { inner }).collect())
    }

    /// Fetch and extract in one step.
    ///
    /// **The result is not 1:1 with the input** — see `fetch`. Match on
    /// `Article.url` rather than by position, or pass `raise_on_error=True`.
    #[pyo3(signature = (urls, *, raise_on_error = false))]
    fn read(
        &self,
        py: Python<'_>,
        urls: Vec<String>,
        raise_on_error: bool,
    ) -> PyResult<Vec<PyArticle>> {
        let results = py.detach(|| runtime().map(|rt| rt.block_on(self.pipeline.read(&urls))))?;
        collect(results, raise_on_error)
            .map(|arts| arts.into_iter().map(|inner| PyArticle { inner }).collect())
    }

    /// Search, read the top results, and compress them into a context window.
    #[pyo3(signature = (query, *, max_sources = 5))]
    fn research(&self, py: Python<'_>, query: &str, max_sources: usize) -> PyResult<PyResearch> {
        let inner = py.detach(|| {
            runtime().map(|rt| rt.block_on(self.pipeline.research(query, max_sources)))
        })?;
        Ok(PyResearch { inner })
    }

    /// Write the cookie jar to the `cookie_file` this client was built with.
    ///
    /// Returns how many cookies were saved, or 0 when no file was configured.
    /// Clearance cookies are the expensive part of getting through a bot wall;
    /// saving them means not paying for them again next run.
    fn save_cookies(&self) -> PyResult<usize> {
        Ok(self.pipeline.fetcher().save_cookies()?)
    }

    fn __repr__(&self) -> String {
        self.summary.clone()
    }
}

fn collect<T>(results: Vec<crate::error::Result<T>>, raise: bool) -> PyResult<Vec<T>> {
    let mut out = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(v) => out.push(v),
            Err(e) if raise => return Err(e.into()),
            Err(_) => {}
        }
    }
    Ok(out)
}

// ----------------------------------------------------------- free functions

fn parse_index_mode(mode: &str) -> PyResult<IndexMode> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(IndexMode::Auto),
        "never" | "off" => Ok(IndexMode::Never),
        "always" | "on" => Ok(IndexMode::Always),
        other => Err(PyValueError::new_err(format!(
            "index_mode must be `auto`, `never` or `always`, not {other:?}"
        ))),
    }
}

/// Catch the most likely first mistake: handing `extract` a URL.
///
/// The library fetches, so `extract("https://…")` is a natural thing to try,
/// and HTML that happens to be a bare URL is not a thing that exists. Left
/// alone it returns an empty `Article` and the caller has nothing to go on.
fn reject_url_as_html(html: &str) -> PyResult<()> {
    let trimmed = html.trim();
    let looks_like_url = trimmed.len() < 2048
        && !trimmed.contains('<')
        && !trimmed.contains(char::is_whitespace)
        && (trimmed.starts_with("http://") || trimmed.starts_with("https://"));
    if looks_like_url {
        return Err(PyValueError::new_err(format!(
            "expected HTML, got what looks like a URL ({trimmed:?}).\n\
             `extract` takes markup you already have and does not fetch. To \
             read a URL:\n    rustai.Client().read([{trimmed:?}])[0]\n\
             or, for a whole question:\n    rustai.research(\"your question\")"
        )));
    }
    Ok(())
}

/// Denoise a raw HTML string into Markdown. No network access.
#[pyfunction]
#[pyo3(signature = (html, url = None, *, include_links = true, include_images = false, include_tables = true, index_mode = "auto"))]
fn extract(
    py: Python<'_>,
    html: &str,
    url: Option<&str>,
    include_links: bool,
    include_images: bool,
    include_tables: bool,
    index_mode: &str,
) -> PyResult<PyArticle> {
    reject_url_as_html(html)?;
    let opts = ExtractOptions {
        render: RenderOptions { include_links, include_images, include_tables },
        denoise: DenoiseConfig { keep_tables: include_tables, ..Default::default() },
        index_mode: parse_index_mode(index_mode)?,
        ..ExtractOptions::new()
    };
    // Extraction on a large document is long enough to be worth releasing the
    // GIL for, and it touches nothing Python owns.
    let inner = py.detach(|| crate::parse::extract_with(html, url, &opts))?;
    Ok(PyArticle { inner })
}

/// Denoise many HTML strings at once, in parallel across the rayon pool.
///
/// This is the batch form of [`extract`]. Parsing is CPU-bound and per-document
/// independent, so a list of 20 documents costs roughly one document's wall
/// time on a multi-core machine — and because the GIL is released for the whole
/// call, it parallelises whether or not the caller uses threads.
///
/// `urls` is optional; when given it must be the same length as `documents`.
/// Output is 1:1 with the input, so a failed document raises rather than
/// silently shifting every later index.
#[pyfunction]
#[pyo3(signature = (documents, urls = None, *, include_links = true, include_images = false, include_tables = true, index_mode = "auto"))]
fn extract_many(
    py: Python<'_>,
    documents: Vec<String>,
    urls: Option<Vec<Option<String>>>,
    include_links: bool,
    include_images: bool,
    include_tables: bool,
    index_mode: &str,
) -> PyResult<Vec<PyArticle>> {
    for doc in &documents {
        reject_url_as_html(doc)?;
    }
    if let Some(urls) = &urls
        && urls.len() != documents.len()
    {
        return Err(PyValueError::new_err(format!(
            "urls has {} entries but documents has {}",
            urls.len(),
            documents.len()
        )));
    }
    let opts = ExtractOptions {
        render: RenderOptions { include_links, include_images, include_tables },
        denoise: DenoiseConfig { keep_tables: include_tables, ..Default::default() },
        index_mode: parse_index_mode(index_mode)?,
        ..ExtractOptions::new()
    };

    let results = py.detach(|| {
        let pairs: Vec<(&str, Option<&str>)> = documents
            .iter()
            .enumerate()
            .map(|(i, html)| {
                let url = urls.as_ref().and_then(|u| u[i].as_deref());
                (html.as_str(), url)
            })
            .collect();
        crate::parse::extract_many(pairs, &opts)
    });

    results.into_iter().map(|r| r.map(|inner| PyArticle { inner }).map_err(PyErr::from)).collect()
}

/// Rank and compress already-extracted articles into a context window.
#[pyfunction]
#[pyo3(signature = (query, articles, *, max_tokens = 2048, diversity = 0.35, max_tokens_per_source = None, include_breadcrumbs = true))]
fn slim(
    py: Python<'_>,
    query: &str,
    articles: Vec<PyArticle>,
    max_tokens: usize,
    diversity: f32,
    max_tokens_per_source: Option<usize>,
    include_breadcrumbs: bool,
) -> PyResult<PyContext> {
    if !(0.0..=1.0).contains(&diversity) {
        return Err(PyValueError::new_err("diversity must be between 0.0 and 1.0"));
    }
    let cfg = SlimConfig {
        max_tokens,
        diversity,
        max_tokens_per_source,
        include_breadcrumbs,
        ..Default::default()
    };
    let owned: Vec<Article> = articles.into_iter().map(|a| a.inner).collect();
    let inner = py.detach(|| crate::rank::slim(query, &owned, &cfg));
    Ok(PyContext { inner })
}

/// Search, read and compress in one call, using a throwaway client.
#[pyfunction]
#[pyo3(signature = (query, *, max_sources = 5, max_tokens = 2048, providers = None, impersonate = "chrome", respect_robots = true, contact_email = None))]
#[allow(clippy::too_many_arguments)]
fn research(
    py: Python<'_>,
    query: &str,
    max_sources: usize,
    max_tokens: usize,
    providers: Option<Vec<String>>,
    impersonate: &str,
    respect_robots: bool,
    contact_email: Option<String>,
) -> PyResult<PyResearch> {
    let client = PyClient::new(
        providers,
        16,
        20.0,
        impersonate,
        respect_robots,
        0.25,
        2,
        8 * 1024 * 1024,
        "en-US,en;q=0.9",
        false,
        contact_email,
        None,
        60.0,
        None,
        max_tokens,
        0.35,
        true,
        false,
        true,
        "auto",
        10,
    )?;
    client.research(py, query, max_sources)
}

/// Estimate how many LLM tokens a string costs.
#[pyfunction]
fn count_tokens(text: &str) -> usize {
    crate::text::estimate_tokens(text)
}

/// Split text into the tokens the ranker uses.
#[pyfunction]
fn tokenize(text: &str) -> Vec<String> {
    crate::text::tokenize(text)
}

/// Score a block of text for information density, `0.0`–`1.0`.
#[pyfunction]
fn density(text: &str) -> f32 {
    crate::rank::density(text).score
}

/// Normalise a URL to the identity used for deduplication.
#[pyfunction]
fn canonical_url(url: &str) -> Option<String> {
    crate::search::canonical_key(url)
}

/// Parse an RSS or Atom feed into a list of dicts.
#[pyfunction]
fn parse_feed(py: Python<'_>, xml: &str) -> PyResult<Py<PyAny>> {
    to_dict(py, &crate::search::parse_rss(xml)?)
}

/// Parse a sitemap into `{"urls": [...], "sitemaps": [...]}`.
#[pyfunction]
fn parse_sitemap(py: Python<'_>, xml: &str) -> PyResult<Py<PyAny>> {
    to_dict(py, &crate::search::parse_sitemap(xml)?)
}

/// The compiled extension module.
#[pymodule]
#[pyo3(name = "_rustai")]
pub fn rustai_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    m.add_class::<PyClient>()?;
    m.add_class::<PyArticle>()?;
    m.add_class::<PyPage>()?;
    m.add_class::<PyUnit>()?;
    m.add_class::<PyLink>()?;
    m.add_class::<PyMeta>()?;
    m.add_class::<PyStats>()?;
    m.add_class::<PySearchResult>()?;
    m.add_class::<PyContext>()?;
    m.add_class::<PyResearch>()?;

    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(extract_many, m)?)?;
    m.add_function(wrap_pyfunction!(slim, m)?)?;
    m.add_function(wrap_pyfunction!(research, m)?)?;
    m.add_function(wrap_pyfunction!(count_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(tokenize, m)?)?;
    m.add_function(wrap_pyfunction!(density, m)?)?;
    m.add_function(wrap_pyfunction!(canonical_url, m)?)?;
    m.add_function(wrap_pyfunction!(parse_feed, m)?)?;
    m.add_function(wrap_pyfunction!(parse_sitemap, m)?)?;

    m.add("RustaiError", m.py().get_type::<RustaiError>())?;
    m.add("NetworkError", m.py().get_type::<NetworkError>())?;
    m.add("HttpStatusError", m.py().get_type::<HttpStatusError>())?;
    m.add("RobotsError", m.py().get_type::<RobotsError>())?;
    m.add("ExtractError", m.py().get_type::<ExtractError>())?;
    m.add("ProviderError", m.py().get_type::<ProviderError>())?;
    m.add("BrowserError", m.py().get_type::<BrowserError>())?;

    Ok(())
}

#[cfg(test)]
mod guard_tests {
    use super::reject_url_as_html;

    #[test]
    fn rejects_a_bare_url() {
        assert!(reject_url_as_html("https://example.com/x").is_err());
        assert!(reject_url_as_html("  http://example.com  ").is_err());
    }

    /// Everything that is plausibly markup, or prose, has to pass -- including
    /// a document that merely mentions a URL, and one with no tags at all.
    #[test]
    fn passes_anything_that_could_be_a_document() {
        assert!(reject_url_as_html("<p>hi</p>").is_ok());
        assert!(reject_url_as_html("").is_ok());
        assert!(reject_url_as_html("just some prose").is_ok());
        assert!(reject_url_as_html("see https://example.com for more").is_ok());
        assert!(reject_url_as_html("<a href=\"https://example.com\">x</a>").is_ok());
        assert!(reject_url_as_html("ftp://example.com/file").is_ok());
    }
}
