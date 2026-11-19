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
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{Mutex, Semaphore};
use url::Url;
use wreq::dns::{Addrs, GaiResolver, Name, Resolve, Resolving};

use crate::error::{Error, Result};
use robots::Robots;

/// Default identity when impersonation is off, or for the `robots.txt` lookup.
pub const DEFAULT_USER_AGENT: &str =
    concat!("rustai/", env!("CARGO_PKG_VERSION"), " (+https://github.com/imhyensuk/rustai)");

/// The token we match `robots.txt` groups against.
pub const ROBOTS_AGENT: &str = "rustai";

/// Hard cap on a `robots.txt`.
///
/// Every other body is capped while streaming; this one is fetched before a
/// crawl can start, so an endless `robots.txt` would be a free way to exhaust
/// a crawler's memory. Google stops reading at 500 KB and nothing legitimate
/// comes close.
const MAX_ROBOTS_BYTES: usize = 512 * 1024;

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
#[derive(Clone)]
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
    /// Allow loopback, private and link-local addresses as fetch targets.
    ///
    /// Off by default. A search result or a redirect can point at
    /// `169.254.169.254` or at a service bound to the machine running this,
    /// and whatever comes back is extracted into a context window destined for
    /// a model -- which turns an ordinary crawl into a way to read the cloud
    /// metadata endpoint out loud. Turn it on to crawl your own development
    /// server.
    pub allow_private_hosts: bool,
}

/// Hand-written rather than derived: [`FetchConfig::proxies`] can carry
/// `user:pass@`, and a derived `Debug` would put those credentials into any log
/// that dumps a `Fetcher` or a `Pipeline`. Shows the fields worth seeing in a
/// dump; `..` covers the rest, so a new field cannot reintroduce a leak.
impl std::fmt::Debug for FetchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchConfig")
            .field("concurrency", &self.concurrency)
            .field("timeout", &self.timeout)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("retries", &self.retries)
            .field("respect_robots", &self.respect_robots)
            .field("impersonate", &self.impersonate)
            .field("browser_fallback", &self.browser_fallback)
            .field("proxies", &self.proxies.iter().map(|p| mask_proxy(p)).collect::<Vec<_>>())
            .field("cookie_file", &self.cookie_file)
            .field("allow_private_hosts", &self.allow_private_hosts)
            .finish_non_exhaustive()
    }
}

/// Replace any `user:pass@` in a proxy URL with `***`, keeping scheme, host and
/// port — which are what a reader actually needs to identify the proxy.
fn mask_proxy(proxy: &str) -> String {
    let Ok(mut parsed) = Url::parse(proxy) else { return proxy.to_string() };
    if parsed.username().is_empty() && parsed.password().is_none() {
        return proxy.to_string();
    }
    let _ = parsed.set_username("***");
    let _ = parsed.set_password(Some("***"));
    parsed.to_string()
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
            allow_private_hosts: false,
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
                // Followed in `fetch_following` instead: see there for why.
                .redirect(wreq::redirect::Policy::none())
                .pool_max_idle_per_host(cfg.concurrency.min(32))
                .referer(true)
                .gzip(true)
                .brotli(true)
                .deflate(true)
                .zstd(true);

            // Literal addresses are already refused by `Fetcher::guard`; this
            // is the same judgement applied to what a hostname resolves to.
            if !cfg.allow_private_hosts {
                builder = builder.dns_resolver(Arc::new(PublicOnlyDns(GaiResolver::new())));
            }

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

    /// Refuse an address only this machine or this network can reach.
    ///
    /// Every path that turns a `Url` into a request calls this, rather than
    /// the entry point alone: `wreq` follows HTTP 3xx behind our back (handled
    /// by [`redirect_policy`]), and a `<meta refresh>` is followed by the loop
    /// in [`Fetcher::fetch_inner`], which reaches the network through
    /// [`Fetcher::get_with_retry`] without passing [`normalize`] again.
    fn guard(&self, url: &Url) -> Result<()> {
        if !self.cfg.allow_private_hosts && is_internal(url) {
            return Err(Error::InvalidUrl(format!("{url} is an internal address")));
        }
        Ok(())
    }

    /// Follow HTTP redirects to a page, checking every hop.
    ///
    /// `wreq` is set to [`wreq::redirect::Policy::none`] so the chain is walked
    /// here instead. All three things owed to a redirect target need `await`
    /// and so cannot live in a redirect policy, which is a synchronous
    /// closure: `robots.txt` has to be read for the host the redirect actually
    /// lands on, the internal-address guard has to see each target, and pacing
    /// is per-host. Before this, a 3xx to a disallowed path on another host
    /// was fetched without ever consulting that host's `robots.txt`.
    async fn fetch_following(&self, start: &Url, check_robots: bool) -> Result<Page> {
        let mut current = start.clone();
        for _ in 0..=self.cfg.max_redirects {
            self.guard(&current)?;
            let crawl_delay = if check_robots {
                let robots = self.robots_for(&current).await;
                if !robots.allows(current.path()) {
                    return Err(Error::RobotsDenied(current.to_string()));
                }
                robots.crawl_delay
            } else {
                None
            };
            self.pace(&current, crawl_delay).await;
            match self.get_with_retry(&current).await? {
                Hop::Page(page) => return Ok(page),
                Hop::Redirect(next) => current = next,
            }
        }
        Err(Error::network(start, format!("more than {} redirects", self.cfg.max_redirects)))
    }

    async fn fetch_inner(&self, url: &str, check_robots: bool) -> Result<Page> {
        let parsed = normalize(url)?;
        // Early, so an internal target costs neither a permit nor a robots fetch.
        self.guard(&parsed)?;
        let _permit = self.gate.acquire().await.map_err(|e| Error::Config(e.to_string()))?;

        let started = Instant::now();
        let mut page = self.fetch_following(&parsed, check_robots).await?;

        // HTML-level redirects. A stub that redirects via `<meta refresh>` or
        // `location.replace` returns a perfectly good 200 containing no
        // content -- which is what sites that canonicalise in the browser do.
        let mut current = parsed.clone();
        for _ in 0..MAX_HTML_REDIRECTS {
            let Some(next) = detect_html_redirect(&page.body, &current) else { break };
            // The stub is still a better answer than an error, so a refused or
            // disallowed target ends the chain rather than failing the fetch.
            let Ok(followed) = self.fetch_following(&next, check_robots).await else { break };
            page = followed;
            current = next;
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
    async fn get_with_retry(&self, url: &Url) -> Result<Hop> {
        self.guard(url)?;
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

    async fn get_once(&self, url: &Url) -> Result<Hop> {
        let resp =
            self.client().get(url.as_str()).send().await.map_err(|e| Error::network(url, e))?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        let content_type = resp
            .headers()
            .get(wreq::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        // `Policy::none()` hands the 3xx back rather than following it. A
        // redirect with no usable target is just a failed request.
        if (300..400).contains(&status)
            && let Some(next) = resp
                .headers()
                .get(wreq::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .filter(|next| matches!(next.scheme(), "http" | "https"))
        {
            return Ok(Hop::Redirect(next));
        }
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
        let bytes = self.read_capped(url, resp, self.cfg.max_body_bytes).await?;
        let body = decode(&bytes, charset.as_deref(), tld_of(&final_url).as_deref());

        Ok(Hop::Page(Page {
            url: url.to_string(),
            final_url,
            status,
            content_type,
            body,
            elapsed_ms: 0,
            rendered: false,
        }))
    }

    /// Read a body, aborting as soon as it crosses `limit`.
    async fn read_capped(&self, url: &Url, resp: wreq::Response, limit: usize) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::network(url, e))?;
            if buf.len() + chunk.len() > limit {
                return Err(Error::BodyTooLarge { url: url.to_string(), limit });
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
        let key = robots_origin(url);
        if let Some(cached) = self.robots.lock().await.get(&key) {
            return cached.clone();
        }
        let robots_url = format!("{key}/robots.txt");
        let parsed = match self.client().get(&robots_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match self.read_capped(url, resp, MAX_ROBOTS_BYTES).await {
                    // RFC 9309 says UTF-8; a host that disagrees gets lossy
                    // decoding rather than an unparsed file.
                    Ok(bytes) => Robots::parse(&String::from_utf8_lossy(&bytes), ROBOTS_AGENT),
                    Err(_) => Robots::allow_all(),
                }
            }
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
        self.guard(url)?;
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
    // Clearance and session cookies are credentials: on Unix the file is born
    // 0600 rather than chmod'd afterwards, so it is never briefly world-readable.
    // `mode` only applies to a file this call creates, hence the unlink first.
    #[cfg(unix)]
    let written = {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(path);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| f.write_all(json.as_bytes()))
    };
    #[cfg(not(unix))]
    let written = std::fs::write(path, &json);
    written.map_err(|e| Error::Config(format!("could not write {}: {e}", path.display())))?;
    Ok(stored.len())
}

/// What one request produced: the page, or a redirect still to follow.
enum Hop {
    Page(Page),
    Redirect(Url),
}

/// Whether an address is one only this machine or this network can reach.
fn is_internal_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00 // unique local, fc00::/7
                || (ip.segments()[0] & 0xffc0) == 0xfe80 // link-local, fe80::/10
                || ip.to_ipv4_mapped().is_some_and(|v4| is_internal_ip(&IpAddr::V4(v4)))
        }
    }
}

/// Whether a URL names somewhere only this machine or this network can reach.
///
/// What the URL *says*, which is all that can be judged before resolving it.
/// A hostname pointing at an internal address is caught by [`PublicOnlyDns`]
/// instead, at the moment it is resolved.
fn is_internal(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => is_internal_ip(&IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_internal_ip(&IpAddr::V6(ip)),
        // `.internal` is reserved for private use and is where GCP parks its
        // metadata service; a trailing dot names the same host.
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost") || name.ends_with(".internal")
        }
        None => false,
    }
}

/// A resolver that drops internal addresses before anything is dialled.
///
/// [`is_internal`] can only read the URL. A public hostname whose record
/// points at `127.0.0.1` passes it, and so does one whose record flips to an
/// internal address *after* the check and before the connection -- DNS
/// rebinding. Filtering here closes both, because `wreq` dials the addresses a
/// resolver hands back: the address that was vetted is the address used, with
/// no second lookup in between to disagree with the first.
///
/// A name that resolves to a mix keeps its public addresses. One that resolves
/// only to internal ones fails to resolve at all, which is the truthful answer
/// -- there is nowhere this is willing to connect.
///
/// Generic over the inner resolver so a test can supply the answers rather
/// than depending on what the network says today.
struct PublicOnlyDns<R>(R);

impl<R: Resolve + 'static> Resolve for PublicOnlyDns<R> {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let inner = self.0.resolve(name);
        Box::pin(async move {
            let kept: Vec<SocketAddr> =
                inner.await?.filter(|addr| !is_internal_ip(&addr.ip())).collect();
            if kept.is_empty() {
                return Err(format!("{host} resolves only to internal addresses").into());
            }
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}

/// The origin whose `robots.txt` governs `url`, as a cache key and a base.
///
/// RFC 9309 scopes the file to an origin, so the port is part of it:
/// `example.com:8080` has its own `robots.txt` and deserves its own cache
/// entry. Keying on scheme and host alone fetched port 80's file for every
/// non-default port, and when nothing answered there the miss read as "allow
/// all" -- which silently disabled `robots.txt` for those hosts entirely.
fn robots_origin(url: &Url) -> String {
    url.origin().ascii_serialization()
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

/// Decode a body, trusting the header, then a `<meta charset>`, then UTF-8,
/// and only when the bytes are not UTF-8 either, a guess.
///
/// Legacy encodings are still common outside the anglosphere — EUC-KR and
/// Shift_JIS in particular — and mojibake destroys every downstream heuristic,
/// so this is worth getting right rather than assuming UTF-8. Old pages often
/// declare nothing at all (math.snu.ac.kr serves EUC-KR that way), so the last
/// step is the detector Firefox uses, hinted with the host's top-level domain
/// the way a browser is.
fn decode(bytes: &[u8], header_charset: Option<&str>, tld: Option<&str>) -> String {
    let label = header_charset.map(str::to_string).or_else(|| sniff_meta_charset(bytes));
    let encoding = label
        .as_deref()
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or_else(|| {
            // A capped body can end inside a multi-byte character; up to three
            // trailing bytes of invalid UTF-8 are a cut, not another encoding.
            if bytes.len() - encoding_rs::Encoding::utf8_valid_up_to(bytes) <= 3 {
                encoding_rs::UTF_8
            } else {
                let mut detector = chardetng::EncodingDetector::new();
                detector.feed(bytes, true);
                detector.guess(tld.map(str::as_bytes), true)
            }
        });
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// The last label of a URL's host — the hint the encoding detector takes.
fn tld_of(url: &str) -> Option<String> {
    let host = Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    host.rsplit('.').next().map(str::to_string)
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
        assert_eq!(decode(&bytes, Some("euc-kr"), None), "한글 인코딩");
        // Without the header, the meta tag is the fallback.
        let mut doc = b"<html><head><meta charset=\"euc-kr\"></head><body>".to_vec();
        doc.extend_from_slice(&bytes);
        assert!(decode(&doc, None, None).contains("한글 인코딩"));
    }

    #[test]
    fn guesses_a_legacy_encoding_that_nothing_declares() {
        // math.snu.ac.kr serves EUC-KR with neither a header charset nor a meta tag.
        let (bytes, _, _) = encoding_rs::EUC_KR.encode(concat!(
            "<html><head><title>합성함수 미분법 = 연쇄법칙(Chain Rule)</title></head><body>",
            "<p>함수 y = f(x) 와 함수 z = g(y)를 합성한 함수의 미분법은 다음과 같다.</p></body></html>",
        ));
        assert!(decode(&bytes, None, Some("kr")).contains("합성함수 미분법 = 연쇄법칙"));
    }

    #[test]
    fn undeclared_utf8_stays_utf8() {
        assert_eq!(
            decode("<p>한글 UTF-8 본문</p>".as_bytes(), None, Some("kr")),
            "<p>한글 UTF-8 본문</p>"
        );
    }

    #[test]
    fn a_body_cut_mid_character_is_still_read_as_utf8() {
        // The body cap can stop a fetch inside a multi-byte character.
        let full = "<p>가나다라마바사</p>".repeat(20);
        let cut = &full.as_bytes()[..full.len() - 7];
        assert!(decode(cut, None, Some("kr")).starts_with("<p>가나다라마바사</p>"));
    }

    #[test]
    fn tld_is_the_last_host_label() {
        assert_eq!(tld_of("https://www.math.snu.ac.kr/a.htm").as_deref(), Some("kr"));
        assert_eq!(tld_of("not a url"), None);
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

    #[cfg(unix)]
    #[test]
    fn a_saved_jar_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rustai-cookie-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jar.json");

        // Start from a world-readable file: saving has to replace the mode, not
        // inherit it, or a jar written once stays exposed for every later save.
        std::fs::write(&path, "[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let jar = wreq::cookie::Jar::default();
        jar.add("cf_clearance=secret; Path=/", "https://example.com/");
        save_jar(&jar, &["example.com".to_string()], &path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cookie jar left at {mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn robots_is_scoped_to_an_origin_including_its_port() {
        let key = |u: &str| robots_origin(&Url::parse(u).unwrap());
        // The port belongs in the key. Without it a non-default port fetched
        // port 80's file, found nothing, and read the miss as "allow all".
        assert_eq!(key("http://example.com:8080/a/b"), "http://example.com:8080");
        assert_ne!(key("http://example.com:8080/a"), key("http://example.com:9090/a"));
        // A default port is not spelled out, so it shares one entry with a
        // URL that omits it -- which is the same origin.
        assert_eq!(key("https://example.com/a"), "https://example.com");
        assert_eq!(key("https://example.com:443/a"), key("https://example.com/b"));
        assert_eq!(key("http://example.com:80/a"), key("http://example.com/b"));
        // Scheme still separates them.
        assert_ne!(key("http://example.com/a"), key("https://example.com/a"));
    }

    /// A resolver that answers with whatever it was built from.
    struct Fixed(Vec<SocketAddr>);

    impl Resolve for Fixed {
        fn resolve(&self, _: Name) -> Resolving {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(Box::new(addrs.into_iter()) as Addrs) })
        }
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("socket addr")
    }

    #[tokio::test]
    async fn resolving_drops_internal_addresses() {
        // The rebinding case: a name that looks public and answers otherwise.
        // `is_internal` cannot see this, because the URL says nothing wrong.
        let only_internal = PublicOnlyDns(Fixed(vec![addr("127.0.0.1:80")]));
        assert!(
            only_internal.resolve(Name::from("rebind.test")).await.is_err(),
            "a name with nowhere public to go must not resolve"
        );

        // A mixed answer keeps the public addresses and loses the rest, so one
        // poisoned record cannot redirect a connection inward.
        let mixed = PublicOnlyDns(Fixed(vec![
            addr("127.0.0.1:80"),
            addr("93.184.216.34:80"),
            addr("10.0.0.7:80"),
            addr("[::1]:80"),
            addr("[2606:4700::1]:80"),
        ]));
        let kept: Vec<SocketAddr> =
            mixed.resolve(Name::from("mixed.test")).await.expect("resolves").collect();
        assert_eq!(kept, vec![addr("93.184.216.34:80"), addr("[2606:4700::1]:80")]);
    }

    #[test]
    fn internal_ip_ranges_are_recognised() {
        let internal = [
            "127.0.0.1",
            "127.9.9.9",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
        ];
        for ip in internal {
            let ip: IpAddr = ip.parse().expect(ip);
            assert!(is_internal_ip(&ip), "{ip} should be internal");
        }

        let public = ["8.8.8.8", "93.184.216.34", "172.32.0.1", "2606:4700::1", "::ffff:8.8.8.8"];
        for ip in public {
            let ip: IpAddr = ip.parse().expect(ip);
            assert!(!is_internal_ip(&ip), "{ip} should be reachable");
        }
    }

    #[test]
    fn internal_addresses_are_recognised() {
        let internal = [
            "http://127.0.0.1/",
            "http://localhost:3000/",
            "http://sub.localhost/",
            "http://LOCALHOST./",
            "http://169.254.169.254/latest/meta-data/", // AWS metadata
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
        ];
        for u in internal {
            assert!(is_internal(&Url::parse(u).unwrap()), "{u} should be internal");
        }

        let public = [
            "https://example.com/",
            "https://en.wikipedia.org/wiki/BM25",
            "http://8.8.8.8/",
            "http://172.32.0.1/", // just outside 172.16/12
            "http://[2606:4700::1]/",
            "https://notlocalhost.com/", // suffix match must not be a substring match
            "https://internal.example.com/",
        ];
        for u in public {
            assert!(!is_internal(&Url::parse(u).unwrap()), "{u} should be reachable");
        }
    }

    #[tokio::test]
    async fn fetching_an_internal_address_is_refused_before_the_network() {
        let f = Fetcher::new().unwrap();
        let err = f.fetch("http://169.254.169.254/latest/meta-data/").await.unwrap_err();
        assert!(matches!(err, Error::InvalidUrl(_)), "got {err:?}");
        // `fetch_api` skips robots, not this.
        assert!(f.fetch_api("http://127.0.0.1:8080/").await.is_err());

        // Opt in and the gate is gone -- the fetch then fails on connection,
        // which is a different error entirely.
        let cfg = FetchConfig { allow_private_hosts: true, ..Default::default() };
        let f = Fetcher::with_config(cfg).unwrap();
        let err = f.fetch("http://127.0.0.1:9/").await.unwrap_err();
        assert!(!matches!(err, Error::InvalidUrl(_)), "gate still firing: {err:?}");
    }

    #[tokio::test]
    async fn every_request_path_shares_one_internal_address_guard() {
        let f = Fetcher::new().unwrap();
        let internal = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        // `fetch_inner` gates the entry point, but a `<meta refresh>` hop and a
        // headless render reach the network through their own calls -- so the
        // guard lives where they all pass, not only at the door.
        assert!(f.guard(&internal).is_err());
        assert!(f.get_with_retry(&internal).await.is_err());
        assert!(f.guard(&Url::parse("https://example.com/").unwrap()).is_ok());

        let open =
            Fetcher::with_config(FetchConfig { allow_private_hosts: true, ..Default::default() })
                .unwrap();
        assert!(open.guard(&internal).is_ok());
    }

    #[test]
    fn a_meta_refresh_to_an_internal_address_is_still_an_internal_address() {
        // `detect_html_redirect` only vets the scheme; the guard is what stops
        // the hop, so the two have to agree on what this URL is.
        let base = Url::parse("https://evil.test/post").unwrap();
        let body = r#"<!doctype html><meta http-equiv="refresh"
            content="0;url=http://169.254.169.254/latest/meta-data/">"#;
        let next = detect_html_redirect(body, &base).expect("redirect should parse");
        assert!(is_internal(&next), "{next} must be caught by the guard");
    }

    #[test]
    fn proxy_credentials_do_not_reach_debug_output() {
        // `Url` normalises an empty path to `/` on the way out; this string is
        // only ever displayed, never handed back to the proxy builder.
        assert_eq!(
            mask_proxy("http://user:hunter2@proxy.dev:8080"),
            "http://***:***@proxy.dev:8080/"
        );
        // Nothing to hide, nothing rewritten.
        assert_eq!(mask_proxy("socks5://127.0.0.1:1080"), "socks5://127.0.0.1:1080");
        assert_eq!(mask_proxy("not a url"), "not a url");

        let cfg = FetchConfig {
            proxies: vec!["http://bob:s3cr3t@proxy.dev:8080".into()],
            ..Default::default()
        };
        let dumped = format!("{cfg:?}");
        assert!(!dumped.contains("s3cr3t"), "password leaked: {dumped}");
        assert!(dumped.contains("proxy.dev"), "host should survive: {dumped}");
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
