//! HTTP status and error mapping for the OpenAI API.

use std::time::Duration;

use harness_ports::ProviderError;
use reqwest::header::HeaderMap;
use serde_json::Value;

/// Maps a non-success HTTP response to a [`ProviderError`].
///
/// A 429 caused by an exhausted quota (`insufficient_quota`) is not retryable,
/// so it is reported as an invalid request rather than a rate limit.
pub(crate) fn error_from_status(
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
) -> ProviderError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = parsed["error"]["message"]
        .as_str()
        .map_or_else(|| body.chars().take(300).collect(), str::to_owned);
    let code = parsed["error"]["code"].as_str().unwrap_or_default();
    let retry_after_ms = retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    match status {
        401 | 403 => ProviderError::Auth(message),
        // Waiting does not refill a quota: report it as a permanent error.
        429 if code == "insufficient_quota" => ProviderError::InvalidRequest { status, message },
        429 => ProviderError::RateLimited { retry_after_ms },
        500..=599 => ProviderError::Server { status, message },
        _ => ProviderError::InvalidRequest { status, message },
    }
}

/// Maps an error `code` from a stream event or failed response.
pub(crate) fn error_from_code(code: &str, message: &str) -> ProviderError {
    let message = message.to_owned();
    match code {
        "rate_limit_exceeded" => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        "server_error" | "overloaded" | "internal_error" => ProviderError::Server {
            status: 500,
            message,
        },
        _ => ProviderError::InvalidRequest {
            status: 400,
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

    fn body(code: &str) -> String {
        format!(r#"{{"error":{{"message":"msg","type":"t","param":null,"code":"{code}"}}}}"#)
    }

    #[test]
    fn statuses_map_to_error_kinds() {
        assert_eq!(
            error_from_status(401, &body("invalid_api_key"), None),
            ProviderError::Auth("msg".into())
        );
        assert_eq!(
            error_from_status(
                429,
                &body("rate_limit_exceeded"),
                Some(Duration::from_secs(2))
            ),
            ProviderError::RateLimited {
                retry_after_ms: Some(2000)
            }
        );
        assert_eq!(
            error_from_status(503, &body("x"), None),
            ProviderError::Server {
                status: 503,
                message: "msg".into()
            }
        );
        assert_eq!(
            error_from_status(404, &body("model_not_found"), None),
            ProviderError::InvalidRequest {
                status: 404,
                message: "msg".into()
            }
        );
    }

    #[test]
    fn exhausted_quota_is_not_retryable() {
        let e = error_from_status(429, &body("insufficient_quota"), None);
        assert!(!e.is_retryable(), "{e:?}");
        assert!(e.to_string().contains("msg"));
    }

    #[test]
    fn stream_error_codes_map_to_error_kinds() {
        assert_eq!(
            error_from_code("rate_limit_exceeded", "m"),
            ProviderError::RateLimited {
                retry_after_ms: None
            }
        );
        assert_eq!(
            error_from_code("server_error", "m"),
            ProviderError::Server {
                status: 500,
                message: "m".into()
            }
        );
        assert_eq!(
            error_from_code("invalid_prompt", "m"),
            ProviderError::InvalidRequest {
                status: 400,
                message: "m".into()
            }
        );
    }

    #[test]
    fn retry_after_headers_are_parsed() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("3"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(3)));
        h.insert("retry-after-ms", HeaderValue::from_static("20"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_millis(20)));
    }
}
