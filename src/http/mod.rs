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
    concat!("rustai/", env!("CARGO_PKG_VERSION"), " (+https://github.com/imhyensuk/rustai)");

/// The token we match `robots.txt` groups against.
pub const ROBOTS_AGENT: &str = "rustai";

/// HTML-level redirects followed before giving up.
///
/// Two is enough for the real pattern (a canonicalising stub pointing at the
/// article) and low enough that a redirect loop costs almost nothing.
const MAX_HTML_REDIRECTS: usize = 2;

/// Above this much extracted text, a page is not an empty shell.
const MAX_STATIC_TEXT: usize = 2_000;

/// One character of text per this many bytes of markup is the floor below which
/// a page is presumed to be rendered client-side.
const TEXT_TO_MARKUP_FLOOR: usize = 200;

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
    /// Proxies to route requests through, rotated round-robin.
    ///
    /// The only real answer to IP-reputation blocking: a TLS fingerprint says
    /// what your client is, and an address says who it is. Accepts `http://`,
    /// `https://` and `socks5://` URLs, with optional `user:pass@`.
    pub proxies: Vec<String>,
    /// Honour a `Retry-After` header, up to this long. Set to zero to ignore it.
    pub max_retry_after: Duration,
    /// File to persist cookies to, so a session survives the process.
    pub cookie_file: Option<std::path::PathBuf>,
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
            proxies: Vec::new(),
            max_retry_after: Duration::from_secs(60),
            cookie_file: None,
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
    /// One client per proxy, or a single direct client. Rotated per request.
    clients: Arc<Vec<wreq::Client>>,
    next_client: Arc<std::sync::atomic::AtomicUsize>,
    jar: Option<Arc<wreq::cookie::Jar>>,
    /// Hosts this fetcher has touched. The cookie jar indexes by domain but
    /// does not expose that key, so a host-only cookie — which is most session
    /// cookies — cannot be attributed on the way back out without this.
    visited: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
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
        let jar = cfg.cookie_file.as_ref().map(|path| Arc::new(load_jar(path)));

        let build_one = |proxy: Option<&str>| -> Result<wreq::Client> {
            let mut builder = wreq::Client::builder()
                .timeout(cfg.timeout)
                .connect_timeout(cfg.connect_timeout)
                .redirect(wreq::redirect::Policy::limited(cfg.max_redirects))
                .pool_max_idle_per_host(cfg.concurrency.min(32))
                .referer(true)
                .gzip(true)
                .brotli(true)
                .deflate(true)
                .zstd(true);

            builder = match &jar {
                Some(jar) => builder.cookie_provider(jar.clone()),
                None => builder.cookie_store(true),
            };
            builder = apply_impersonation(builder, &cfg.impersonate)?;

            if let Some(proxy) = proxy {
                let proxy = wreq::Proxy::all(proxy)
                    .map_err(|e| Error::Config(format!("invalid proxy `{proxy}`: {e}")))?;
                builder = builder.proxy(proxy);
            }

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
            builder.build().map_err(|e| Error::Config(e.to_string()))
        };

        let clients = if cfg.proxies.is_empty() {
            vec![build_one(None)?]
        } else {
            cfg.proxies.iter().map(|p| build_one(Some(p))).collect::<Result<Vec<_>>>()?
        };

        Ok(Fetcher {
            clients: Arc::new(clients),
            next_client: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            jar,
            visited: Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new())),
            gate: Arc::new(Semaphore::new(cfg.concurrency)),
            cfg: Arc::new(cfg),
            robots: Arc::new(Mutex::new(HashMap::new())),
            last_hit: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The next client in the rotation.
    ///
    /// Round-robin rather than random: with a small proxy pool, randomness
    /// clusters, and clustering on one address is exactly what gets it banned.
    fn client(&self) -> &wreq::Client {
        if self.clients.len() == 1 {
            return &self.clients[0];
        }
        let i = self.next_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        &self.clients[i % self.clients.len()]
    }

    /// Write the cookie jar to [`FetchConfig::cookie_file`].
    ///
    /// Clearance cookies are the expensive part of getting through a bot wall;
    /// throwing them away when the process exits means paying for them again.
    pub fn save_cookies(&self) -> Result<usize> {
        let (Some(jar), Some(path)) = (&self.jar, &self.cfg.cookie_file) else { return Ok(0) };
        let hosts: Vec<String> = match self.visited.lock() {
            Ok(v) => v.iter().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
        };
        save_jar(jar, &hosts, path)
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
        let mut page = self.get_with_retry(&parsed).await?;

        // Follow HTML-level redirects. `wreq` follows HTTP 3xx, but a stub page
        // that redirects via `<meta refresh>` or `location.replace` returns a
        // perfectly good 200 containing no content — which is exactly what
        // happens on sites that canonicalise URLs in the browser.
        let mut current = parsed.clone();
        for _ in 0..MAX_HTML_REDIRECTS {
            let Some(next) = detect_html_redirect(&page.body, &current) else { break };
            if check_robots && !self.robots_for(&next).await.allows(next.path()) {
                break;
            }
            self.pace(&next, crawl_delay).await;
            match self.get_with_retry(&next).await {
                Ok(followed) => {
                    page = followed;
                    current = next;
                }
                // The stub is still a better answer than an error.
                Err(_) => break,
            }
        }
        page.url = parsed.to_string();
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

    /// One URL, with exponential backoff on retryable failures.
    async fn get_with_retry(&self, url: &Url) -> Result<Page> {
        let mut attempt = 0u32;
        loop {
            match self.get_once(url).await {
                Ok(page) => return Ok(page),
                Err(e) if e.is_retryable() && attempt < self.cfg.retries => {
                    // Retrying a 429 immediately is how a client turns a soft
                    // limit into a hard block. When the server says how long to
                    // wait, that beats any backoff curve we could invent — but
                    // an hour-long hint is a refusal, not a delay, so it is
                    // capped and otherwise treated as a failure.
                    let wait = match e.retry_after() {
                        Some(hint) if hint <= self.cfg.max_retry_after => hint,
                        Some(_) => return Err(e),
                        None => self.cfg.retry_backoff * 2u32.pow(attempt),
                    };
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn get_once(&self, url: &Url) -> Result<Page> {
        let resp =
            self.client().get(url.as_str()).send().await.map_err(|e| Error::network(url, e))?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        let content_type = resp
            .headers()
            .get(wreq::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        if !(200..300).contains(&status) {
            let retry_after = resp
                .headers()
                .get(wreq::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after);
            return Err(Error::Status { url: url.to_string(), status, retry_after });
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
        let parsed = match self.client().get(&robots_url).send().await {
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
        if let Ok(mut visited) = self.visited.lock() {
            visited.insert(host.clone());
        }
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

/// Parse a `Retry-After` value into seconds.
///
/// Only the delta-seconds form. The HTTP-date form is rare in practice and
/// parsing it would mean carrying a date library for one header; an unparsed
/// value simply falls back to exponential backoff.
fn parse_retry_after(value: &str) -> Option<f64> {
    let secs: f64 = value.trim().parse().ok()?;
    secs.is_finite().then_some(secs.max(0.0))
}

/// One persisted cookie, with the host it belongs to.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCookie {
    name: String,
    value: String,
    /// Host the cookie was collected from.
    host: String,
    path: String,
    /// `Domain` attribute, when the cookie carried one. Absent means host-only,
    /// and reloading must keep it that way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
}

/// Read a cookie jar from disk. A missing or unreadable file yields an empty
/// jar: a lost session is a slow start, never an error.
fn load_jar(path: &std::path::Path) -> wreq::cookie::Jar {
    let jar = wreq::cookie::Jar::default();
    let Ok(raw) = std::fs::read_to_string(path) else { return jar };
    let Ok(stored) = serde_json::from_str::<Vec<StoredCookie>>(&raw) else { return jar };
    for c in stored {
        let uri = format!("https://{}{}", c.host, c.path);
        let mut cookie = format!("{}={}; Path={}", c.name, c.value, c.path);
        if let Some(domain) = &c.domain {
            cookie.push_str(&format!("; Domain={domain}"));
        }
        jar.add(cookie.as_str(), uri.as_str());
    }
    jar
}

/// Write a cookie jar to disk, returning how many cookies were saved.
fn save_jar(jar: &wreq::cookie::Jar, hosts: &[String], path: &std::path::Path) -> Result<usize> {
    // Asking the jar what it would send to each host we visited is the only way
    // to recover a host-only cookie's owner: `get_all` returns the cookies but
    // not the domain key they were filed under.
    let mut seen = std::collections::HashSet::new();
    let mut stored: Vec<StoredCookie> = Vec::new();
    for host in hosts {
        for c in jar.matches(format!("https://{host}/").as_str()) {
            let cookie = StoredCookie {
                name: c.name().to_string(),
                value: c.value().to_string(),
                host: host.clone(),
                path: c.path().unwrap_or("/").to_string(),
                domain: c.domain().map(str::to_string),
            };
            if seen.insert((cookie.host.clone(), cookie.name.clone(), cookie.path.clone())) {
                stored.push(cookie);
            }
        }
    }
    let json = serde_json::to_string_pretty(&stored)
        .map_err(|e| Error::Config(format!("could not serialise cookies: {e}")))?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, json)
        .map_err(|e| Error::Config(format!("could not write {}: {e}", path.display())))?;
    Ok(stored.len())
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

/// Find an HTML-level redirect target in a page body.
///
/// Two forms matter in practice: `<meta http-equiv="refresh" content="0; url=…">`
/// and a script that assigns `location`. Both are gated on the page being small
/// and on a zero delay, because a redirect stub is always tiny — a long article
/// that happens to contain `location.href` somewhere must not be mistaken for
/// one.
fn detect_html_redirect(body: &str, base: &Url) -> Option<Url> {
    const MAX_STUB_BYTES: usize = 4096;
    if body.len() > MAX_STUB_BYTES {
        return None;
    }
    let target = meta_refresh_target(body).or_else(|| script_location_target(body))?;
    let next = base.join(target.trim().trim_matches(['"', '\''])).ok()?;
    if !matches!(next.scheme(), "http" | "https") {
        return None;
    }
    // A stub pointing at itself is a loop, not a redirect.
    let same = next.as_str().trim_end_matches('/') == base.as_str().trim_end_matches('/');
    (!same).then_some(next)
}

/// `<meta http-equiv="refresh" content="0; url=TARGET">`, delay 0 only.
fn meta_refresh_target(body: &str) -> Option<&str> {
    let lower = body.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find("http-equiv") {
        let tag_start = lower[..from + at].rfind('<')?;
        let tag_end = lower[tag_start..].find('>')? + tag_start;
        let tag = &lower[tag_start..tag_end];
        if tag.contains("refresh")
            && let Some(content_at) = tag.find("content")
            && let Some(url_at) = tag[content_at..].find("url=")
        {
            // Only an immediate redirect; a delayed one is a page in its own right.
            let delay: String = tag[content_at..]
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if delay.parse::<u32>().unwrap_or(0) == 0 {
                let start = tag_start + content_at + url_at + 4;
                let rest = &body[start..];
                let end = rest.find(['"', '\'', '>']).unwrap_or(rest.len());
                let target = rest[..end].trim();
                if !target.is_empty() {
                    return Some(target);
                }
            }
        }
        from += at + "http-equiv".len();
    }
    None
}

/// `location.replace("TARGET")`, `location.href = "TARGET"`, and friends.
fn script_location_target(body: &str) -> Option<&str> {
    for marker in ["location.replace(", "location.href=", "location.href =", "location.assign("] {
        let Some(at) = body.find(marker) else { continue };
        let rest = &body[at + marker.len()..];
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            // An indirection through a variable; resolving it needs a JS engine.
            continue;
        }
        let inner = &rest[quote.len_utf8()..];
        let end = inner.find(quote)?;
        let target = &inner[..end];
        if target.starts_with("http") || target.starts_with('/') {
            return Some(target);
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

    // Plenty of text: whatever else the page is doing, we already have it.
    if text_len > MAX_STATIC_TEXT {
        return false;
    }

    // Markup carrying essentially no text. An absolute floor misses the common
    // case — a 350 KB front page yielding 510 characters clears any fixed
    // threshold while plainly being an app shell — so the test is the ratio.
    // Real pages run percent-level text-to-markup; a shell runs a tenth of that.
    let starved = text_len.saturating_mul(TEXT_TO_MARKUP_FLOOR) < html.len();
    if text_len < 120 || starved {
        return true;
    }

    let lower = html.to_lowercase();
    let spa_markers = ["__next_data__", "id=\"root\"", "id=\"app\"", "ng-app", "data-reactroot"];
    spa_markers.iter().any(|m| lower.contains(m))
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
    fn retry_after_is_parsed_and_capped() {
        assert_eq!(parse_retry_after("120"), Some(120.0));
        assert_eq!(parse_retry_after("  0 "), Some(0.0));
        assert_eq!(parse_retry_after("-5"), Some(0.0));
        // The HTTP-date form is not parsed; backoff takes over.
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);

        let e =
            Error::Status { url: "https://x.dev/".into(), status: 429, retry_after: Some(30.0) };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(30)));
        assert!(e.is_retryable());
        let none = Error::Status { url: "https://x.dev/".into(), status: 429, retry_after: None };
        assert_eq!(none.retry_after(), None);
    }

    #[test]
    fn proxies_build_one_client_each() {
        let cfg = FetchConfig {
            proxies: vec!["http://127.0.0.1:8080".into(), "socks5://127.0.0.1:1080".into()],
            ..Default::default()
        };
        let fetcher = Fetcher::with_config(cfg).expect("proxied client");
        assert_eq!(fetcher.clients.len(), 2);
        // Round-robin, so a small pool cannot cluster on one address.
        let first = fetcher.client() as *const _;
        let second = fetcher.client() as *const _;
        assert_ne!(first, second);
        assert_eq!(fetcher.client() as *const _, first);
    }

    #[test]
    fn an_invalid_proxy_is_rejected_at_build_time() {
        let cfg = FetchConfig { proxies: vec!["not a proxy".into()], ..Default::default() };
        assert!(Fetcher::with_config(cfg).is_err());
    }

    #[test]
    fn cookies_survive_a_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("rustai-cookies-{}", std::process::id()));
        let path = dir.join("jar.json");
        let _ = std::fs::remove_file(&path);

        let jar = wreq::cookie::Jar::default();
        // A domain cookie and a host-only one: the latter is what most session
        // cookies actually are, and it has no Domain attribute to save.
        jar.add("cf_clearance=abc123; Domain=example.com; Path=/", "https://example.com/");
        jar.add("session=xyz789; Path=/", "https://example.com/");
        let hosts = vec!["example.com".to_string()];
        assert_eq!(save_jar(&jar, &hosts, &path).unwrap(), 2);

        let reloaded = load_jar(&path);
        assert!(reloaded.contains("session", "https://example.com/"), "host-only cookie lost");
        assert!(reloaded.contains("cf_clearance", "https://example.com/"), "cookie was lost");
        assert_eq!(
            reloaded.get("cf_clearance", "https://example.com/").map(|c| c.value().to_string()),
            Some("abc123".to_string())
        );

        // A missing file is a slow start, not an error.
        assert_eq!(load_jar(&dir.join("nope.json")).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fetcher_without_a_cookie_file_saves_nothing() {
        let fetcher = Fetcher::new().unwrap();
        assert_eq!(fetcher.save_cookies().unwrap(), 0);
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
    fn follows_a_meta_refresh_stub() {
        let base = Url::parse("https://blog.dev/2024/02/08/Post.html").unwrap();
        let body = r#"<!doctype html><title>Redirect</title>
            <noscript><meta http-equiv="refresh" content="0; url=https://blog.dev/2024/02/08/Post/"></noscript>"#;
        assert_eq!(
            detect_html_redirect(body, &base).unwrap().as_str(),
            "https://blog.dev/2024/02/08/Post/"
        );
    }

    #[test]
    fn follows_a_script_location_stub() {
        let base = Url::parse("https://blog.dev/a.html").unwrap();
        let body = r#"<!doctype html><title>Redirect</title><script>
            const target = "https://blog.dev/a/";
            window.location.replace("https://blog.dev/a/");
        </script>"#;
        assert_eq!(detect_html_redirect(body, &base).unwrap().as_str(), "https://blog.dev/a/");
    }

    #[test]
    fn resolves_a_relative_redirect_target() {
        let base = Url::parse("https://blog.dev/x/a.html").unwrap();
        let body = r#"<meta http-equiv="refresh" content="0;url=/y/b.html">"#;
        assert_eq!(
            detect_html_redirect(body, &base).unwrap().as_str(),
            "https://blog.dev/y/b.html"
        );
    }

    #[test]
    fn ignores_delayed_refreshes_and_self_loops() {
        let base = Url::parse("https://blog.dev/a").unwrap();
        // A 5-second refresh is a real page that reloads, not a redirect.
        assert!(
            detect_html_redirect(
                r#"<meta http-equiv="refresh" content="5; url=https://blog.dev/b">"#,
                &base
            )
            .is_none()
        );
        // Pointing at itself is a loop.
        assert!(
            detect_html_redirect(
                r#"<meta http-equiv="refresh" content="0; url=https://blog.dev/a">"#,
                &base
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_a_long_page_that_merely_mentions_location() {
        let base = Url::parse("https://blog.dev/a").unwrap();
        let article = format!(
            "<article><p>{}</p><p>You can call location.replace(\"https://evil.dev/\") in JS.</p></article>",
            "Real prose. ".repeat(500)
        );
        assert!(
            detect_html_redirect(&article, &base).is_none(),
            "a full article was treated as a stub"
        );
    }

    #[test]
    fn ignores_non_http_and_unresolvable_targets() {
        let base = Url::parse("https://blog.dev/a").unwrap();
        assert!(
            detect_html_redirect(
                r#"<meta http-equiv="refresh" content="0;url=javascript:x()">"#,
                &base
            )
            .is_none()
        );
        assert!(detect_html_redirect("<p>nothing here</p>", &base).is_none());
        // A variable indirection needs a JS engine; we must not guess.
        assert!(
            detect_html_redirect(r#"<script>window.location.replace(target);</script>"#, &base)
                .is_none()
        );
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

#[cfg(test)]
mod js_gate_tests {
    use super::*;

    fn page(body: String) -> Page {
        Page {
            url: "https://x.dev/".into(),
            final_url: "https://x.dev/".into(),
            status: 200,
            content_type: Some("text/html".into()),
            body,
            elapsed_ms: 0,
            rendered: false,
        }
    }

    /// Modelled on a real front page whose stories render client-side: ~350 KB
    /// of markup carrying ~500 characters of text. An absolute text threshold
    /// waves that through — 500 clears any sane floor — while the ratio does
    /// not. Index detection cannot rescue such a page either: there is nothing
    /// in the HTML to index.
    #[test]
    fn a_client_rendered_front_page_is_flagged() {
        let shell = format!(
            "<html><body><nav>{}</nav><main></main><script>{}</script></body></html>",
            "<a href=\"/section\">Section link</a>".repeat(20),
            "x".repeat(340_000)
        );
        assert!(needs_javascript(&page(shell)));
    }

    /// The same size of page, but the markup actually carries the article.
    #[test]
    fn a_large_real_article_is_not_flagged() {
        let real = format!(
            "<html><body><article><h1>T</h1><p>{}</p></article></body></html>",
            "Real prose that a reader can actually read and act on. ".repeat(400)
        );
        assert!(!needs_javascript(&page(real)));
    }
}
