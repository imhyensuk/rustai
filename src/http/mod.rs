//! The fetch layer: impersonated HTTP, politeness, and a headless fallback.
//!
//! Three things separate this from a bare `reqwest` loop:
//!
//! * **Fingerprint.** Cloudflare and friends read the TLS ClientHello (JA3/JA4)
//!   and the HTTP/2 SETTINGS frame, not just the User-Agent. `wreq` replays a
//!   real Chrome's, so a plain `GET` gets a plain `200` where a stock client
//!   gets a challenge page.
//! * **Politeness.** `robots.txt`, per-host spacing and a concurrency ceiling
//!   are on by default, because a fast crawler with none of those is a way to
//!   get a user's IP banned.
//! * **Escalation.** JS-gated pages are detected from the response and, when
//!   the `browser` feature is on, retried through headless Chrome — which is
//!   two orders of magnitude more expensive and therefore never the first move.

pub mod robots;

#[cfg(feature = "browser")]
pub mod browser;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use crate::error::{Error, Result};
use robots::Robots;

/// Default identity when impersonation is off, or for the `robots.txt` lookup.
pub const DEFAULT_USER_AGENT: &str =
    concat!("rustai/", env!("CARGO_PKG_VERSION"), " (+https://github.com/hyeonseok-im/rustai)");

/// The token we match `robots.txt` groups against.
pub const ROBOTS_AGENT: &str = "rustai";

/// Content types we are willing to parse as a document.
const TEXTUAL: &[&str] = &[
    "text/html",
    "application/xhtml",
    "text/plain",
    "application/xml",
    "text/xml",
    "application/rss",
    "application/atom",
    "application/json",
];

/// How the client should present itself on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Impersonate {
    /// A recent Chrome build. The default, and what almost everyone wants.
    #[default]
    Chrome,
    /// A recent Firefox build.
    Firefox,
    /// A recent Safari build.
    Safari,
    /// A named `wreq-util` profile, e.g. `"chrome_143"` or `"safari_ios_26"`.
    Named(String),
    /// Market-share weighted random profile, re-rolled per client.
    Random,
    /// No emulation: plain TLS and a `rustai/x.y.z` User-Agent.
    None,
}

impl Impersonate {
    /// Parse the string form used by the Python API.
    pub fn parse(s: &str) -> Result<Impersonate> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "off" | "false" => Impersonate::None,
            "chrome" | "true" | "default" => Impersonate::Chrome,
            "firefox" => Impersonate::Firefox,
            "safari" => Impersonate::Safari,
            "random" => Impersonate::Random,
            other => Impersonate::Named(other.to_string()),
        })
    }
}

/// Everything tunable about fetching.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Maximum requests in flight across all hosts.
    pub concurrency: usize,
    /// Total per-request deadline.
    pub timeout: Duration,
    /// Connection establishment deadline.
    pub connect_timeout: Duration,
    /// Redirects followed before giving up.
    pub max_redirects: usize,
    /// Hard cap on a response body. Enforced while streaming, so an oversized
    /// body costs one chunk, not the whole download.
    pub max_body_bytes: usize,
    /// Retries for retryable failures.
    pub retries: u32,
    /// Base delay for exponential backoff between retries.
    pub retry_backoff: Duration,
    /// Minimum spacing between two requests to the same host.
    pub per_host_delay: Duration,
    /// Honour `robots.txt`. Leave this on unless you own the target.
    pub respect_robots: bool,
    /// Honour a `Crawl-delay` larger than [`FetchConfig::per_host_delay`].
    pub respect_crawl_delay: bool,
    /// Browser fingerprint to present.
    pub impersonate: Impersonate,
    /// `Accept-Language` header.
    pub accept_language: String,
    /// Escalate JS-gated pages to headless Chrome. Needs the `browser` feature.
    pub browser_fallback: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        FetchConfig {
            concurrency: 16,
            timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(8),
            max_redirects: 5,
            max_body_bytes: 8 * 1024 * 1024,
            retries: 2,
            retry_backoff: Duration::from_millis(400),
            per_host_delay: Duration::from_millis(250),
            respect_robots: true,
            respect_crawl_delay: true,
            impersonate: Impersonate::Chrome,
            accept_language: "en-US,en;q=0.9".to_string(),
            browser_fallback: false,
        }
    }
}

/// A fetched document.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Page {
    /// URL as requested.
    pub url: String,
    /// URL after redirects.
    pub final_url: String,
    /// HTTP status.
    pub status: u16,
    /// `Content-Type` header, if any.
    pub content_type: Option<String>,
    /// Decoded body.
    pub body: String,
    /// Wall time for the fetch, in milliseconds.
    pub elapsed_ms: u64,
    /// Whether the headless fallback produced this body.
    pub rendered: bool,
}

/// A pooled, polite, fingerprint-aware HTTP client.
#[derive(Clone)]
pub struct Fetcher {
    client: wreq::Client,
    cfg: Arc<FetchConfig>,
    gate: Arc<Semaphore>,
    robots: Arc<Mutex<HashMap<String, Arc<Robots>>>>,
    last_hit: Arc<Mutex<HashMap<String, Instant>>>,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher").field("config", &self.cfg).finish_non_exhaustive()
    }
}

impl Fetcher {
    /// Build a fetcher with default settings.
    pub fn new() -> Result<Self> {
        Self::with_config(FetchConfig::default())
    }

    /// Build a fetcher from an explicit config.
    pub fn with_config(cfg: FetchConfig) -> Result<Self> {
        if cfg.concurrency == 0 {
            return Err(Error::Config("concurrency must be at least 1".into()));
        }
        let mut builder = wreq::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(cfg.connect_timeout)
            .redirect(wreq::redirect::Policy::limited(cfg.max_redirects))
            .pool_max_idle_per_host(cfg.concurrency.min(32))
            .cookie_store(true)
            .referer(true)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .zstd(true);

        builder = apply_impersonation(builder, &cfg.impersonate)?;

        let mut headers = wreq::header::HeaderMap::new();
        if let Ok(v) = wreq::header::HeaderValue::from_str(&cfg.accept_language) {
            headers.insert(wreq::header::ACCEPT_LANGUAGE, v);
        }
        if matches!(cfg.impersonate, Impersonate::None) {
            headers.insert(
                wreq::header::USER_AGENT,
                wreq::header::HeaderValue::from_static(DEFAULT_USER_AGENT),
            );
        }
        builder = builder.default_headers(headers);

        let client = builder.build().map_err(|e| Error::Config(e.to_string()))?;
        Ok(Fetcher {
            client,
            gate: Arc::new(Semaphore::new(cfg.concurrency)),
            cfg: Arc::new(cfg),
            robots: Arc::new(Mutex::new(HashMap::new())),
            last_hit: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The configuration this fetcher was built with.
    pub fn config(&self) -> &FetchConfig {
        &self.cfg
    }

    /// Fetch one URL, honouring `robots.txt` when configured to.
    pub async fn fetch(&self, url: &str) -> Result<Page> {
        self.fetch_inner(url, self.cfg.respect_robots).await
    }

    /// Fetch a provider endpoint, skipping the `robots.txt` check.
    ///
    /// `robots.txt` governs crawling — the discovery of documents a site did
    /// not hand you. A search or feed endpoint the caller named explicitly is
    /// a query, not a crawl, and several of them (DuckDuckGo's HTML endpoint
    /// among them) disallow the very path they exist to serve. Pages found
    /// *through* those endpoints go through [`Fetcher::fetch`] and are checked
    /// normally, which is where the rule actually protects anyone.
    pub async fn fetch_api(&self, url: &str) -> Result<Page> {
        self.fetch_inner(url, false).await
    }

    async fn fetch_inner(&self, url: &str, check_robots: bool) -> Result<Page> {
        let parsed = normalize(url)?;
        let _permit = self.gate.acquire().await.map_err(|e| Error::Config(e.to_string()))?;

        let crawl_delay = if check_robots {
            let robots = self.robots_for(&parsed).await;
            if !robots.allows(parsed.path()) {
                return Err(Error::RobotsDenied(parsed.to_string()));
            }
            robots.crawl_delay
        } else {
            None
        };
        self.pace(&parsed, crawl_delay).await;

        let started = Instant::now();
        let mut attempt = 0u32;
        let page = loop {
            match self.get_once(&parsed).await {
                Ok(page) => break page,
                Err(e) if e.is_retryable() && attempt < self.cfg.retries => {
                    // Exponential backoff. Retrying a 429 immediately is how a
                    // client turns a soft limit into a hard block.
                    let wait = self.cfg.retry_backoff * 2u32.pow(attempt);
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        };

        let mut page = page;
        page.elapsed_ms = started.elapsed().as_millis() as u64;

        if self.cfg.browser_fallback && needs_javascript(&page) {
            // A failed escalation is not a failed fetch: the static body is
            // still the best answer we have.
            if let Ok(rendered) = self.render(&parsed).await {
                page.body = rendered;
                page.rendered = true;
                page.elapsed_ms = started.elapsed().as_millis() as u64;
            }
        }
        Ok(page)
    }

    /// Fetch many URLs concurrently, preserving input order in the output.
    ///
    /// The concurrency ceiling and per-host spacing still apply, so this is
    /// safe to call with a few hundred URLs at once.
    pub async fn fetch_many(&self, urls: &[String]) -> Vec<Result<Page>> {
        let futures = urls.iter().map(|u| {
            let this = self.clone();
            let u = u.clone();
            async move { this.fetch(&u).await }
        });
        futures_util::stream::iter(futures).buffered(self.cfg.concurrency).collect::<Vec<_>>().await
    }

    async fn get_once(&self, url: &Url) -> Result<Page> {
        let resp =
            self.client.get(url.as_str()).send().await.map_err(|e| Error::network(url, e))?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        let content_type = resp
            .headers()
            .get(wreq::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        if !(200..300).contains(&status) {
            return Err(Error::Status { url: url.to_string(), status });
        }
        if let Some(ct) = &content_type {
            let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            if !TEXTUAL.iter().any(|t| base.starts_with(t)) {
                return Err(Error::Parse(format!("{url}: unsupported content type {base}")));
            }
        }
        if resp.content_length().is_some_and(|n| n as usize > self.cfg.max_body_bytes) {
            return Err(Error::BodyTooLarge {
                url: url.to_string(),
                limit: self.cfg.max_body_bytes,
            });
        }

        let charset = content_type.as_deref().and_then(charset_of);
        let bytes = self.read_capped(url, resp).await?;
        let body = decode(&bytes, charset.as_deref());

        Ok(Page {
            url: url.to_string(),
            final_url,
            status,
            content_type,
            body,
            elapsed_ms: 0,
            rendered: false,
        })
    }

    /// Read a body, aborting as soon as it crosses the cap.
    async fn read_capped(&self, url: &Url, resp: wreq::Response) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::network(url, e))?;
            if buf.len() + chunk.len() > self.cfg.max_body_bytes {
                return Err(Error::BodyTooLarge {
                    url: url.to_string(),
                    limit: self.cfg.max_body_bytes,
                });
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    /// Fetch and cache `robots.txt` for a host.
    ///
    /// A missing or unreadable file is treated as "allow all", which is what
    /// the RFC specifies for a 404.
    async fn robots_for(&self, url: &Url) -> Arc<Robots> {
        let host = url.host_str().unwrap_or_default().to_string();
        let key = format!("{}://{host}", url.scheme());
        if let Some(cached) = self.robots.lock().await.get(&key) {
            return cached.clone();
        }
        let robots_url = format!("{key}/robots.txt");
        let parsed = match self.client.get(&robots_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(body) => Robots::parse(&body, ROBOTS_AGENT),
                Err(_) => Robots::allow_all(),
            },
            _ => Robots::allow_all(),
        };
        let parsed = Arc::new(parsed);
        self.robots.lock().await.insert(key, parsed.clone());
        parsed
    }

    /// Space requests to one host apart, honouring `Crawl-delay` when larger.
    async fn pace(&self, url: &Url, crawl_delay: Option<f64>) {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut delay = self.cfg.per_host_delay;
        if self.cfg.respect_crawl_delay
            && let Some(d) = crawl_delay
        {
            delay = delay.max(Duration::from_secs_f64(d.min(30.0)));
        }
        if delay.is_zero() {
            return;
        }
        let wait = {
            let mut map = self.last_hit.lock().await;
            let now = Instant::now();
            let wait = match map.get(&host) {
                Some(prev) => delay.checked_sub(now.duration_since(*prev)),
                None => None,
            };
            // Reserve our slot before releasing the lock, so concurrent callers
            // to the same host queue up instead of all sleeping the same amount.
            map.insert(host, now + wait.unwrap_or_default());
            wait
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
    }

    #[cfg(feature = "browser")]
    async fn render(&self, url: &Url) -> Result<String> {
        browser::render(url.as_str(), self.cfg.timeout).await
    }

    #[cfg(not(feature = "browser"))]
    async fn render(&self, _url: &Url) -> Result<String> {
        Err(Error::Browser("build rustai with the `browser` feature".into()))
    }
}

fn apply_impersonation(
    builder: wreq::ClientBuilder,
    mode: &Impersonate,
) -> Result<wreq::ClientBuilder> {
    #[cfg(not(feature = "impersonate"))]
    {
        if !matches!(mode, Impersonate::None) {
            return Err(Error::Config(
                "browser impersonation needs the `impersonate` feature".into(),
            ));
        }
        Ok(builder)
    }
    #[cfg(feature = "impersonate")]
    {
        use wreq_util::{Emulation, Profile};
        let profile = match mode {
            Impersonate::None => return Ok(builder),
            Impersonate::Random => return Ok(builder.emulation(Emulation::weighted_random())),
            Impersonate::Chrome => Profile::Chrome149,
            Impersonate::Firefox => Profile::Firefox147,
            Impersonate::Safari => Profile::Safari26_4,
            Impersonate::Named(name) => named_profile(name)?,
        };
        Ok(builder.emulation(profile))
    }
}

/// Resolve a `wreq-util` profile from its wire name, e.g. `"chrome_143"`.
///
/// The enum has no `FromStr`, but it does derive `Deserialize` with exactly
/// these renames under the `emulation-serde` feature, so serde is the
/// supported way in — and it stays correct as upstream adds profiles.
#[cfg(feature = "impersonate")]
fn named_profile(name: &str) -> Result<wreq_util::Profile> {
    serde_json::from_value::<wreq_util::Profile>(serde_json::Value::String(name.to_string()))
        .map_err(|_| {
            Error::Config(format!(
                "unknown impersonation profile `{name}`; try `chrome`, `firefox`, `safari`, \
                 `random`, or a versioned name such as `chrome_143`"
            ))
        })
}

/// Reject anything that is not an absolute http(s) URL.
pub fn normalize(url: &str) -> Result<Url> {
    let parsed = Url::parse(url.trim()).map_err(|_| Error::InvalidUrl(url.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl(url.to_string()));
    }
    if parsed.host_str().is_none() {
        return Err(Error::InvalidUrl(url.to_string()));
    }
    Ok(parsed)
}

fn charset_of(content_type: &str) -> Option<String> {
    content_type
        .split(';')
        .skip(1)
        .filter_map(|p| p.trim().strip_prefix("charset="))
        .map(|c| c.trim_matches(['"', '\'']).to_ascii_lowercase())
        .next()
}

/// Decode a body, trusting the header, then a `<meta charset>`, then UTF-8.
///
/// Legacy encodings are still common outside the anglosphere — EUC-KR and
/// Shift_JIS in particular — and mojibake destroys every downstream heuristic,
/// so this is worth getting right rather than assuming UTF-8.
fn decode(bytes: &[u8], header_charset: Option<&str>) -> String {
    let label = header_charset.map(str::to_string).or_else(|| sniff_meta_charset(bytes));
    let encoding = label
        .as_deref()
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Look for `<meta charset=...>` in the first 4 KiB, as browsers do.
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    if let Some(at) = text.find("charset=") {
        let rest = &text[at + "charset=".len()..];
        let value: String = rest
            .trim_start_matches(['"', '\'', ' '])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Does this response look like an empty shell waiting for JavaScript?
///
/// Deliberately conservative. Rendering a page costs ~100x a fetch, so the
/// bar is "there is markup but essentially no text", not "the page mentions
/// JavaScript".
pub fn needs_javascript(page: &Page) -> bool {
    let html = &page.body;
    if html.len() < 512 {
        return false;
    }
    let Ok(article) = crate::parse::extract(html, Some(&page.final_url)) else {
        return true;
    };
    let text_len = crate::text::visible_len(&article.text);
    if text_len > 400 {
        return false;
    }
    let lower = html.to_lowercase();
    let spa_markers = ["__next_data__", "id=\"root\"", "id=\"app\"", "ng-app", "data-reactroot"];
    text_len < 120 || spa_markers.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(normalize("file:///etc/passwd").is_err());
        assert!(normalize("javascript:alert(1)").is_err());
        assert!(normalize("not a url").is_err());
        assert!(normalize("https://example.com/a").is_ok());
    }

    #[test]
    fn parses_charset_from_content_type() {
        assert_eq!(charset_of("text/html; charset=EUC-KR").as_deref(), Some("euc-kr"));
        assert_eq!(charset_of("text/html").as_deref(), None);
    }

    #[test]
    fn decodes_legacy_korean_encoding() {
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("한글 인코딩");
        assert_eq!(decode(&bytes, Some("euc-kr")), "한글 인코딩");
        // Without the header, the meta tag is the fallback.
        let mut doc = b"<html><head><meta charset=\"euc-kr\"></head><body>".to_vec();
        doc.extend_from_slice(&bytes);
        assert!(decode(&doc, None).contains("한글 인코딩"));
    }

    #[test]
    fn impersonate_strings_parse() {
        assert_eq!(Impersonate::parse("chrome").unwrap(), Impersonate::Chrome);
        assert_eq!(Impersonate::parse("none").unwrap(), Impersonate::None);
        assert_eq!(
            Impersonate::parse("chrome_143").unwrap(),
            Impersonate::Named("chrome_143".into())
        );
    }

    #[cfg(feature = "impersonate")]
    #[test]
    fn named_profiles_resolve_and_reject() {
        assert!(named_profile("chrome_143").is_ok());
        assert!(named_profile("netscape_2").is_err());
    }

    #[test]
    fn client_builds_with_every_preset() {
        for mode in ["chrome", "firefox", "safari", "random", "none"] {
            let cfg = FetchConfig {
                impersonate: Impersonate::parse(mode).unwrap(),
                ..Default::default()
            };
            assert!(Fetcher::with_config(cfg).is_ok(), "{mode} failed to build");
        }
    }

    #[test]
    fn zero_concurrency_is_rejected() {
        let cfg = FetchConfig { concurrency: 0, ..Default::default() };
        assert!(Fetcher::with_config(cfg).is_err());
    }

    fn page(body: &str) -> Page {
        Page {
            url: "https://x.dev/".into(),
            final_url: "https://x.dev/".into(),
            status: 200,
            content_type: Some("text/html".into()),
            body: body.into(),
            elapsed_ms: 0,
            rendered: false,
        }
    }

    #[test]
    fn detects_an_spa_shell() {
        let shell = format!(
            "<html><body><div id=\"root\"></div><script>{}</script></body></html>",
            "x".repeat(2000)
        );
        assert!(needs_javascript(&page(&shell)));
    }

    #[test]
    fn does_not_escalate_a_real_article() {
        let real = format!(
            "<html><body><article><h1>T</h1><p>{}</p></article></body></html>",
            "Real prose that a reader can actually read. ".repeat(20)
        );
        assert!(!needs_javascript(&page(&real)));
    }
}
