//! [`OpenAiProvider`]: the HTTP client that implements [`Provider`].

use std::collections::VecDeque;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use harness_ports::{
    ApiKey, Capabilities, ModelRequest, Provider, ProviderError, ProviderEvent, ProviderStream,
    backoff_delay,
};
use tokio_util::sync::CancellationToken;

use crate::http::{error_from_status, parse_retry_after};
use crate::request::PROVIDER_ID;
use crate::{OpenAiSettings, ResponsesDecoder, build_responses_body, default_capabilities};

/// The OpenAI Responses API as a [`Provider`].
pub struct OpenAiProvider {
    settings: OpenAiSettings,
    key: ApiKey,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.settings.base_url)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// Creates a provider.
    ///
    /// # Errors
    /// [`ProviderError::Unsupported`] if the HTTP client cannot be built.
    pub fn new(settings: OpenAiSettings, key: ApiKey) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(settings.connect_timeout)
            .read_timeout(settings.read_timeout)
            .build()
            .map_err(|e| {
                ProviderError::Unsupported(format!("cannot build the HTTP client: {e}"))
            })?;
        Ok(Self {
            settings,
            key,
            client,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.settings.base_url.trim_end_matches('/'))
    }

    /// Sends a request, retrying transient failures until it succeeds, a
    /// permanent error occurs, attempts run out, or `cancel` fires.
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let attempts = self.settings.max_attempts.max(1);
        for attempt in 1..=attempts {
            let request = build().bearer_auth(self.key.expose());
            let result = tokio::select! {
                result = request.send() => result,
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            };
            let error = match result {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry_after = parse_retry_after(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    error_from_status(status, &body, retry_after)
                }
                // reqwest errors carry the URL, never request headers, so the key stays private.
                Err(e) if e.is_timeout() => ProviderError::Timeout,
                Err(e) => ProviderError::Network(e.to_string()),
            };
            if !error.is_retryable() || attempt == attempts {
                return Err(error);
            }
            let delay = backoff_delay(attempt, error.retry_after(), jitter_seed());
            tracing::warn!(attempt, ?delay, %error, "retrying OpenAI request");
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            }
        }
        Err(ProviderError::Unsupported(
            "no request attempts were made".into(),
        ))
    }
}

/// A cheap, non-cryptographic jitter source: the clock's sub-second nanoseconds.
fn jitter_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()))
}

/// State of the decoded event stream.
struct StreamState {
    events: BoxStream<'static, Result<eventsource_stream::Event, String>>,
    decoder: ResponsesDecoder,
    pending: VecDeque<ProviderEvent>,
    cancel: CancellationToken,
    finished: bool,
}

/// Turns server-sent events into provider events, checking `cancel` between events.
fn decode_stream(state: StreamState) -> ProviderStream {
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(event) = st.pending.pop_front() {
                return Some((Ok(event), st));
            }
            if st.finished {
                return None;
            }
            let next = tokio::select! {
                next = st.events.next() => next,
                () = st.cancel.cancelled() => {
                    st.finished = true;
                    return Some((Err(ProviderError::Cancelled), st));
                }
            };
            match next {
                None => {
                    st.finished = true;
                    if !st.decoder.is_done() {
                        return Some((
                            Err(ProviderError::Network(
                                "the response stream ended early".into(),
                            )),
                            st,
                        ));
                    }
                }
                Some(Err(e)) => {
                    st.finished = true;
                    return Some((Err(ProviderError::Network(e)), st));
                }
                Some(Ok(event)) => match st.decoder.feed(&event.event, &event.data) {
                    Ok(events) => st.pending.extend(events),
                    Err(e) => {
                        st.finished = true;
                        return Some((Err(e), st));
                    }
                },
            }
        }
    })
    .boxed()
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    /// Confirms the model exists; limits are conservative defaults (the Models
    /// API does not report them).
    async fn capabilities(&self, model: &str) -> Result<Capabilities, ProviderError> {
        let url = self.url(&format!("/v1/models/{model}"));
        self.send(|| self.client.get(&url), &CancellationToken::new())
            .await?;
        Ok(default_capabilities())
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = build_responses_body(&request, &self.settings)?;
        let url = self.url("/v1/responses");
        let response = self
            .send(
                || {
                    self.client
                        .post(&url)
                        .header("accept", "text/event-stream")
                        .json(&body)
                },
                &cancel,
            )
            .await?;
        let events = response
            .bytes_stream()
            .eventsource()
            .map(|r| r.map_err(|e| e.to_string()))
            .boxed();
        Ok(decode_stream(StreamState {
            events,
            decoder: ResponsesDecoder::new(),
            pending: VecDeque::new(),
            cancel,
            finished: false,
        }))
    }

    /// A local estimate (one token per four characters of instructions and
    /// message text); usage reported by each response is the exact figure.
    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        let chars: usize = request
            .system
            .iter()
            .map(|p| p.text.chars().count())
            .sum::<usize>()
            + request
                .messages
                .iter()
                .map(|m| m.text().chars().count())
                .sum::<usize>();
        Ok(u64::try_from(chars).unwrap_or(u64::MAX).div_ceil(4))
    }
}
