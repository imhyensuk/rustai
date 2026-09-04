//! Error type for the whole pipeline.

use std::fmt;

/// Everything that can go wrong inside `rustai`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The remote host could not be reached, timed out, or returned a
    /// transport-level failure.
    #[error("network error for {url}: {source}")]
    Network {
        /// URL that was being fetched.
        url: String,
        /// Underlying transport error, stringified so the variant stays `Send`.
        source: BoxError,
    },

    /// The server answered, but with a status we treat as a failure.
    #[error("http {status} for {url}")]
    Status {
        /// URL that was being fetched.
        url: String,
        /// The status code returned.
        status: u16,
        /// Seconds the server asked us to wait, from its `Retry-After` header.
        retry_after: Option<f64>,
    },

    /// Response body exceeded [`crate::http::FetchConfig::max_body_bytes`].
    #[error("body for {url} exceeds the {limit} byte cap")]
    BodyTooLarge {
        /// URL that was being fetched.
        url: String,
        /// Configured cap in bytes.
        limit: usize,
    },

    /// `robots.txt` disallows this path for our user-agent.
    #[error("blocked by robots.txt: {0}")]
    RobotsDenied(String),

    /// A URL could not be parsed or was not http(s).
    #[error("invalid url {0}")]
    InvalidUrl(String),

    /// The HTML could not be parsed.
    #[error("html parse error: {0}")]
    Parse(String),

    /// A search or feed provider returned something we could not read.
    #[error("provider `{provider}` failed: {message}")]
    Provider {
        /// Provider name, e.g. `duckduckgo`.
        provider: String,
        /// Human-readable reason.
        message: String,
    },

    /// Headless-browser fallback failed or is not compiled in.
    #[error("headless browser fallback unavailable: {0}")]
    Browser(String),

    /// Invalid configuration supplied by the caller.
    #[error("invalid configuration: {0}")]
    Config(String),
}

/// Boxed, thread-safe source error.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Convenient result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn network(url: impl fmt::Display, e: impl Into<BoxError>) -> Self {
        Error::Network { url: url.to_string(), source: e.into() }
    }

    pub(crate) fn provider(provider: &'static str, message: impl fmt::Display) -> Self {
        Error::Provider { provider: provider.to_string(), message: message.to_string() }
    }

    /// How long the server asked us to wait before trying again, if it said.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Error::Status { retry_after: Some(secs), .. } if *secs >= 0.0 => {
                Some(std::time::Duration::from_secs_f64(*secs))
            }
            _ => None,
        }
    }

    /// Whether retrying the same request could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Network { .. } => true,
            Error::Status { status, .. } => {
                matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
            }
            _ => false,
        }
    }
}
