//! Error types for the RISEx HTTP client.

use std::time::Duration;

use nautilus_network::http::HttpClientError;
use thiserror::Error;

/// Result type for RISEx HTTP operations.
pub type RisexHttpResult<T> = Result<T, RisexHttpError>;

/// Errors returned by the RISEx HTTP client.
#[derive(Debug, Clone, Error)]
pub enum RisexHttpError {
    /// Transport failure or timeout.
    #[error("network error: {0}")]
    Network(String),
    /// Non-success HTTP status without a venue error body.
    #[error("HTTP {status}: {body}")]
    Http {
        status: u16,
        body: String,
        retry_after: Option<Duration>,
    },
    /// HTTP 429.
    #[error("rate limit exceeded: {message}")]
    RateLimit {
        message: String,
        retry_after: Option<Duration>,
    },
    /// Venue error body. `code` is a catalogue code (`E0001`) or a gRPC status name
    /// (`InvalidArgument`).
    #[error("venue error {code}: {message}")]
    Venue { code: String, message: String },
    /// The request was rejected locally, before any call to the venue.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The page cap was reached before the requested range was covered.
    #[error("{data_type} history incomplete after {pages} pages")]
    HistoryIncomplete {
        data_type: &'static str,
        pages: usize,
    },
    /// The response could not be decoded or converted.
    #[error("parse error: {0}")]
    Parse(String),
}

impl RisexHttpError {
    pub(super) const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Http { retry_after, .. } | Self::RateLimit { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl From<HttpClientError> for RisexHttpError {
    fn from(error: HttpClientError) -> Self {
        Self::Network(error.to_string())
    }
}

impl From<serde_json::Error> for RisexHttpError {
    fn from(error: serde_json::Error) -> Self {
        Self::Parse(error.to_string())
    }
}

impl From<anyhow::Error> for RisexHttpError {
    fn from(error: anyhow::Error) -> Self {
        Self::Parse(error.to_string())
    }
}

/// Returns whether a failed request may be sent again.
#[must_use]
pub fn should_retry_risex_http_error(error: &RisexHttpError) -> bool {
    match error {
        RisexHttpError::Network(_) | RisexHttpError::RateLimit { .. } => true,
        RisexHttpError::Http { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Builds the error the retry manager reports when it gives up on time.
#[must_use]
pub fn create_risex_http_timeout_error(message: String) -> RisexHttpError {
    RisexHttpError::Network(message)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn http(status: u16) -> RisexHttpError {
        RisexHttpError::Http {
            status,
            body: String::new(),
            retry_after: Some(Duration::from_secs(2)),
        }
    }

    fn venue() -> RisexHttpError {
        RisexHttpError::Venue {
            code: "InvalidArgument".to_string(),
            message: String::new(),
        }
    }

    #[rstest]
    #[case(RisexHttpError::Network(String::new()), true)]
    #[case(RisexHttpError::RateLimit { message: String::new(), retry_after: None }, true)]
    #[case(http(503), true)]
    #[case(http(404), false)]
    #[case(venue(), false)]
    #[case(RisexHttpError::InvalidRequest(String::new()), false)]
    #[case(RisexHttpError::HistoryIncomplete { data_type: "trade", pages: 1 }, false)]
    #[case(RisexHttpError::Parse(String::new()), false)]
    fn test_should_retry(#[case] error: RisexHttpError, #[case] expected: bool) {
        assert_eq!(should_retry_risex_http_error(&error), expected);
    }

    #[rstest]
    fn test_retry_after_only_on_http_and_rate_limit() {
        assert_eq!(http(503).retry_after(), Some(Duration::from_secs(2)));
        assert_eq!(venue().retry_after(), None);
    }
}
