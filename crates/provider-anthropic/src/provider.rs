//! [`AnthropicProvider`]: the HTTP client that implements [`Provider`].

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use harness_ports::{
    Capabilities, ModelRequest, Provider, ProviderError, ProviderEvent, ProviderStream,
    backoff_delay,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::{error_from_status, parse_retry_after};
use crate::request::PROVIDER_ID;
use crate::{
    AnthropicSettings, ApiKey, StreamDecoder, beta_header, build_count_body, build_messages_body,
    parse_capabilities,
};

/// The Anthropic Messages API as a [`Provider`].
pub struct AnthropicProvider {
    settings: AnthropicSettings,
    key: ApiKey,
    client: reqwest::Client,
    capabilities: Mutex<HashMap<String, Capabilities>>,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.settings.base_url)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Creates a provider.
    ///
    /// # Errors
    /// [`ProviderError::Unsupported`] if the HTTP client cannot be built (for
    /// example no TLS backend is available).
    pub fn new(settings: AnthropicSettings, key: ApiKey) -> Result<Self, ProviderError> {
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
            capabilities: Mutex::default(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.settings.base_url.trim_end_matches('/'))
    }

    fn with_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", &self.settings.api_version)
            .header("anthropic-beta", beta_header(&self.settings))
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
            let result = tokio::select! {
                result = self.with_headers(build()).send() => result,
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
            tracing::warn!(attempt, ?delay, %error, "retrying Anthropic request");
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            }
        }
        Err(ProviderError::Unsupported(
            "no request attempts were made".into(),
        ))
    }

    async fn json(&self, response: reqwest::Response) -> Result<Value, ProviderError> {
        response
            .json::<Value>()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))
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
    decoder: StreamDecoder,
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
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    async fn capabilities(&self, model: &str) -> Result<Capabilities, ProviderError> {
        if let Some(cached) = self
            .capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(model)
        {
            return Ok(cached.clone());
        }
        let url = self.url(&format!("/v1/models/{model}"));
        let response = self
            .send(|| self.client.get(&url), &CancellationToken::new())
            .await?;
        let caps = parse_capabilities(&self.json(response).await?);
        self.capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(model.to_owned(), caps.clone());
        Ok(caps)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = build_messages_body(&request, &self.settings)?;
        let url = self.url("/v1/messages");
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
            decoder: StreamDecoder::new(),
            pending: VecDeque::new(),
            cancel,
            finished: false,
        }))
    }

    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        let body = build_count_body(request, &self.settings)?;
        let url = self.url("/v1/messages/count_tokens");
        let response = self
            .send(
                || self.client.post(&url).json(&body),
                &CancellationToken::new(),
            )
            .await?;
        self.json(response).await?["input_tokens"]
            .as_u64()
            .ok_or_else(|| {
                ProviderError::Decode("count_tokens response has no input_tokens".into())
            })
    }
}
