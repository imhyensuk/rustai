//! A zero-cost local research pipeline: collect, denoise and compress the open
//! web into something a small local model can actually read.
//!
//! No API key, no per-call billing, and no browser-grade DOM in the hot path.
//! The four stages are independent and each is useful on its own:
//!
//! | Stage | Module | What it does |
//! |---|---|---|
//! | Search | [`search`] | free providers, queried concurrently, fused by rank |
//! | Fetch | [`http`] | Chrome TLS/JA3 fingerprint, robots-aware, capped and paced |
//! | Denoise | [`parse`] / [`denoise`] | find the article, drop the furniture, emit Markdown |
//! | Slim | [`rank`] | BM25 + information density + MMR, under a token budget |
//!
//! # The whole pipeline
//!
//! ```no_run
//! # async fn run() -> rustai_core::Result<()> {
//! use rustai_core::pipeline::Pipeline;
//!
//! let pipeline = Pipeline::new()?;
//! let research = pipeline.research("what is BM25", 5).await;
//!
//! println!("{}", research.context.markdown);
//! for (stage, why) in &research.failures {
//!     eprintln!("skipped {stage}: {why}");
//! }
//! # Ok(()) }
//! ```
//!
//! # Just the denoiser
//!
//! No network, no runtime, no configuration:
//!
//! ```
//! let html = r#"
//!     <html><body>
//!       <nav><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a></nav>
//!       <article>
//!         <h1>Arena parsers</h1>
//!         <p>Every node lives in one contiguous buffer, so traversal is a
//!            pointer bump rather than a chase across the heap.</p>
//!       </article>
//!       <footer>© 2026. All rights reserved.</footer>
//!     </body></html>"#;
//!
//! let article = rustai_core::parse::extract(html, Some("https://example.com/post"))?;
//!
//! assert_eq!(article.title(), Some("Arena parsers"));
//! assert!(article.markdown.contains("# Arena parsers"));
//! assert!(!article.text.contains("All rights reserved"));
//! # Ok::<(), rustai_core::Error>(())
//! ```
//!
//! # Just the slimmer
//!
//! Rank blocks you already have against a question, under a token budget that
//! covers the rendered output — headers and breadcrumbs included, not just the
//! blocks themselves:
//!
//! ```
//! use rustai_core::rank::{SlimConfig, slim};
//!
//! let article = rustai_core::parse::extract(
//!     "<article><h1>Numbers</h1>\
//!      <p>Streaming 200 documents held 4.8 MiB resident, at 520 documents per second.</p>\
//!      <p>Paprika is best bloomed in fat before the liquid goes into the pot.</p>\
//!      </article>",
//!     Some("https://example.com/post"),
//! )?;
//!
//! let context = slim("how much memory", &[article], &SlimConfig::with_budget(64));
//!
//! assert!(context.markdown.contains("4.8 MiB"));
//! assert!(context.tokens <= 64);
//! # Ok::<(), rustai_core::Error>(())
//! ```
//!
//! # Being a good citizen
//!
//! [`http::Fetcher`] honours `robots.txt` and `Crawl-delay`, spaces requests to
//! a host, caps concurrency and body size, and backs off exponentially on
//! retry — all by default. Browser impersonation exists so that ordinary
//! reading is not misclassified as abuse; it is not a licence to ignore a
//! site's terms.
//!
//! # Cargo features
//!
//! | Feature | Default | Effect |
//! |---|---|---|
//! | `impersonate` | yes | Chrome TLS/JA3 and HTTP/2 fingerprint emulation |
//! | `python` | no | PyO3 bindings; enabled by maturin |
//! | `browser` | no | headless-Chrome fallback for JS-gated pages |
//! | `nightly-simd` | no | SIMD tokenisation in `tl`; **needs a nightly toolchain** |

#![deny(missing_docs)]

pub mod denoise;
pub mod error;
pub mod http;
pub mod parse;
pub mod pipeline;
pub mod rank;
pub mod search;
pub mod text;

#[cfg(feature = "python")]
mod py;

pub use error::{Error, Result};
