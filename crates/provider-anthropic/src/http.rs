//! HTTP status and error mapping, and `retry-after` parsing (backoff lives in `harness_ports`).

use std::time::Duration;

use harness_ports::ProviderError;
use reqwest::header::HeaderMap;

/// Maps a non-success HTTP response to a [`ProviderError`].
pub(crate) fn error_from_status(
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
) -> ProviderError {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| shorten(body, 300));
    let retry_after_ms = retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    match status {
        401 | 403 => ProviderError::Auth(message),
        429 => ProviderError::RateLimited { retry_after_ms },
        529 => ProviderError::Overloaded { retry_after_ms },
        500..=599 => ProviderError::Server { status, message },
        _ => ProviderError::InvalidRequest { status, message },
    }
}

/// Cuts `text` to at most `max` characters, marking the cut.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}

/// Maps an SSE `error` event (`{"error": {"type", "message"}}`) to a [`ProviderError`].
pub(crate) fn error_from_sse(kind: &str, message: &str) -> ProviderError {
    let message = message.to_owned();
    match kind {
        "overloaded_error" => ProviderError::Overloaded {
            retry_after_ms: None,
        },
        "rate_limit_error" => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        "authentication_error" | "permission_error" => ProviderError::Auth(message),
        "invalid_request_error" | "not_found_error" | "request_too_large" => {
            ProviderError::InvalidRequest {
                status: 400,
                message,
            }
        }
        _ => ProviderError::Server {
            status: 500,
            message,
        },
    }
}

/// Reads `retry-after-ms` (milliseconds) or `retry-after` (whole seconds).
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let number = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok();
    number("retry-after-ms")
        .map(Duration::from_millis)
        .or_else(|| number("retry-after").map(Duration::from_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn statuses_map_to_error_kinds() {
        let body =
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad field"}}"#;
        assert_eq!(
            error_from_status(401, body, None),
            ProviderError::Auth("bad field".into())
        );
        assert_eq!(
            error_from_status(403, body, None),
            ProviderError::Auth("bad field".into())
        );
        assert_eq!(
            error_from_status(429, body, Some(Duration::from_millis(1500))),
            ProviderError::RateLimited {
                retry_after_ms: Some(1500)
            }
        );
        assert_eq!(
            error_from_status(529, body, None),
            ProviderError::Overloaded {
                retry_after_ms: None
            }
        );
        assert_eq!(
            error_from_status(503, body, None),
            ProviderError::Server {
                status: 503,
                message: "bad field".into()
            }
        );
        assert_eq!(
            error_from_status(400, body, None),
            ProviderError::InvalidRequest {
                status: 400,
                message: "bad field".into()
            }
        );
    }

    #[test]
    fn non_json_bodies_are_shortened() {
        let body = "x".repeat(1000);
        match error_from_status(502, &body, None) {
            ProviderError::Server { message, .. } => {
                assert!(message.len() <= 303, "{}", message.len())
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sse_error_types_map_to_error_kinds() {
        assert_eq!(
            error_from_sse("overloaded_error", "x"),
            ProviderError::Overloaded {
                retry_after_ms: None
            }
        );
        assert_eq!(
            error_from_sse("rate_limit_error", "x"),
            ProviderError::RateLimited {
                retry_after_ms: None
            }
        );
        assert_eq!(
            error_from_sse("authentication_error", "x"),
            ProviderError::Auth("x".into())
        );
        assert_eq!(
            error_from_sse("invalid_request_error", "x"),
            ProviderError::InvalidRequest {
                status: 400,
                message: "x".into()
            }
        );
        assert_eq!(
            error_from_sse("api_error", "x"),
            ProviderError::Server {
                status: 500,
                message: "x".into()
            }
        );
    }

    #[test]
    fn retry_after_headers_are_parsed() {
        let mut h = HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None);
        h.insert("retry-after", HeaderValue::from_static("3"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(3)));
        h.insert("retry-after-ms", HeaderValue::from_static("250"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_millis(250)));
        let mut h = HeaderMap::new();
        h.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), None);
    }
}
