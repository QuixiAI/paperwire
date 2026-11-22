//! Outbound HTTP for provider adapters.
//!
//! Adapters depend on the [`Transport`] trait, not on reqwest, so contract
//! tests can run against a local server. The error type answers the one
//! question the operations layer must never get wrong (design.md §19.2):
//! **could the provider have received this request?**
//!
//! [`Retrying`] wraps any transport and retries only when that answer makes
//! it safe.

mod retry;

use std::fmt;
use std::future::Future;
use std::time::Duration;

pub use retry::{RetryPolicy, Retrying};

/// HTTP method. Kept small on purpose: these are the verbs provider APIs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    fn as_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }

    /// Idempotent by HTTP semantics (RFC 9110 §9.2.2).
    pub fn is_idempotent(self) -> bool {
        matches!(self, Self::Get | Self::Put | Self::Delete)
    }
}

/// One part of a `multipart/form-data` body.
#[derive(Clone)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Part")
            .field("name", &self.name)
            .field("filename", &self.filename)
            .field("content_type", &self.content_type)
            .field("len", &self.data.len())
            .finish()
    }
}

#[derive(Clone, Default)]
pub enum Body {
    #[default]
    Empty,
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    Multipart(Vec<Part>),
}

// Bodies carry recipient addresses and document contents; Debug shows only
// their shape.
impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Json(_) => f.write_str("Json(..)"),
            Self::Form(fields) => f
                .debug_tuple("Form")
                .field(&fields.iter().map(|(k, _)| k).collect::<Vec<_>>())
                .finish(),
            Self::Multipart(parts) => f.debug_tuple("Multipart").field(parts).finish(),
        }
    }
}

/// Request credentials. Debug output never shows them (design.md §20.2).
#[derive(Clone)]
pub enum Auth {
    /// Lob uses the API key as the username and no password.
    Basic {
        username: String,
        password: Option<String>,
    },
    /// OAuth access tokens.
    Bearer(String),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Basic { .. } => "Basic(<redacted>)",
            Self::Bearer(_) => "Bearer(<redacted>)",
        })
    }
}

/// A provider-side deduplication key, sent as a request header.
///
/// Setting one is the adapter's declaration that **this provider documents
/// deduplication by this header**: it is what lets [`Retrying`] resend a
/// non-idempotent request. The header name is the provider's, so it is
/// supplied by the adapter rather than assumed here. Never set one for a
/// provider that ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey {
    pub header: String,
    pub value: String,
}

#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    /// Extra headers, e.g. `("Lob-Version", "2020-02-11")`. Credentials go in
    /// [`Request::auth`], not here.
    pub headers: Vec<(String, String)>,
    pub auth: Option<Auth>,
    /// See [`IdempotencyKey`] (design.md §19.1).
    pub idempotency_key: Option<IdempotencyKey>,
    pub body: Body,
    /// Total time for the exchange, body included.
    pub timeout: Duration,
    /// A larger response body fails the request as
    /// [`HttpError::ResponseTooLarge`].
    pub max_response_bytes: usize,
}

impl Request {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
    pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            auth: None,
            idempotency_key: None,
            body: Body::Empty,
            timeout: Self::DEFAULT_TIMEOUT,
            max_response_bytes: Self::DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn basic_auth(mut self, username: impl Into<String>, password: Option<String>) -> Self {
        self.auth = Some(Auth::Basic {
            username: username.into(),
            password,
        });
        self
    }

    pub fn bearer_auth(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(Auth::Bearer(token.into()));
        self
    }

    /// `header` is the provider's documented idempotency header, e.g.
    /// `"Idempotency-Key"` for Lob. See [`IdempotencyKey`].
    pub fn idempotency_key(mut self, header: impl Into<String>, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(IdempotencyKey {
            header: header.into(),
            value: key.into(),
        });
        self
    }

    pub fn json(mut self, value: serde_json::Value) -> Self {
        self.body = Body::Json(value);
        self
    }

    pub fn form(mut self, fields: Vec<(String, String)>) -> Self {
        self.body = Body::Form(fields);
        self
    }

    pub fn multipart(mut self, parts: Vec<Part>) -> Self {
        self.body = Body::Multipart(parts);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }

    /// Whether the provider handling this request twice has the same effect
    /// as handling it once.
    pub fn is_replay_safe(&self) -> bool {
        self.method.is_idempotent() || self.idempotency_key.is_some()
    }
}

// Header values and query strings can hold secrets; Debug shows neither.
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &redact_query(&self.url))
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("auth", &self.auth)
            .field("idempotency_key", &self.idempotency_key)
            .field("body", &self.body)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn redact_query(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<redacted>"),
        None => url.to_owned(),
    }
}

/// A completed HTTP exchange. Any status, including 4xx/5xx, is a response,
/// not an error: the provider answered, so the outcome is known.
#[derive(Clone)]
pub struct Response {
    pub status: u16,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// `Retry-After` in its delay-seconds form. The HTTP-date form yields
    /// `None`, and callers fall back to their own backoff.
    pub fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")?
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("body_len", &self.body.len())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The request was malformed and never left this process.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// No connection was established (DNS, refused, TLS handshake), so the
    /// provider cannot have received the request. Safe to retry.
    #[error("request not sent: {0}")]
    NotSent(String),
    /// The request may have reached the provider, but no complete response
    /// arrived (timeout, reset mid-exchange). **Never retry
    /// blindly**: mark the operation uncertain and reconcile (design.md
    /// §19.2).
    #[error("outcome unknown: {0}")]
    Ambiguous(String),
    /// The provider answered, but the body was larger than
    /// [`Request::max_response_bytes`]. The status and headers in `head`
    /// (its body is empty) still say what the provider did, so a 2xx here
    /// is an accepted request, not an uncertain one. Never retried.
    #[error("status {} response body exceeds {limit} bytes", .head.status)]
    ResponseTooLarge { head: Response, limit: usize },
}

impl HttpError {
    pub fn provider_may_have_received(&self) -> bool {
        matches!(self, Self::Ambiguous(_) | Self::ResponseTooLarge { .. })
    }
}

/// The seam between adapters and the network.
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> impl Future<Output = Result<Response, HttpError>> + Send;
}

/// The production transport.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("paperwire/", env!("CARGO_PKG_VERSION")))
            // A redirect on a POST would silently re-send or drop the body;
            // providers are called at their documented endpoints, so a
            // redirect surfaces as a 3xx response instead of being followed.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| HttpError::InvalidRequest(describe(e)))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    async fn send(&self, request: Request) -> Result<Response, HttpError> {
        let mut builder = self
            .client
            .request(request.method.as_reqwest(), &request.url)
            .timeout(request.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        match &request.auth {
            Some(Auth::Basic { username, password }) => {
                builder = builder.basic_auth(username, password.as_deref());
            }
            Some(Auth::Bearer(token)) => builder = builder.bearer_auth(token),
            None => {}
        }
        if let Some(key) = &request.idempotency_key {
            builder = builder.header(&key.header, &key.value);
        }
        builder = match request.body {
            Body::Empty => builder,
            Body::Json(value) => builder.json(&value),
            Body::Form(fields) => builder.form(&fields),
            Body::Multipart(parts) => {
                let mut form = reqwest::multipart::Form::new();
                for p in parts {
                    let mut part = reqwest::multipart::Part::bytes(p.data);
                    if let Some(filename) = p.filename {
                        part = part.file_name(filename);
                    }
                    if let Some(ct) = p.content_type {
                        part = part.mime_str(&ct).map_err(|e| {
                            HttpError::InvalidRequest(format!("content type {ct:?}: {e}"))
                        })?;
                    }
                    form = form.part(p.name, part);
                }
                builder.multipart(form)
            }
        };

        let mut response = builder.send().await.map_err(classify)?;
        let mut head = Response {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_owned(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: Vec::new(),
        };

        // Headers arrived, so the provider received the request and its
        // status is known. A body that is too large keeps that status; a
        // body that fails mid-read leaves the outcome unknown.
        let limit = request.max_response_bytes;
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(HttpError::ResponseTooLarge { head, limit });
        }
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| HttpError::Ambiguous(describe(e)))?
        {
            if head.body.len() + chunk.len() > limit {
                head.body.clear();
                return Err(HttpError::ResponseTooLarge { head, limit });
            }
            head.body.extend_from_slice(&chunk);
        }
        Ok(head)
    }
}

fn classify(e: reqwest::Error) -> HttpError {
    if e.is_builder() {
        HttpError::InvalidRequest(describe(e))
    } else if e.is_connect() {
        HttpError::NotSent(describe(e))
    } else {
        // Timeout or failure after the connection was up: the request may
        // have been written in full.
        HttpError::Ambiguous(describe(e))
    }
}

/// The error and its causes, without the URL: some providers put
/// credentials in query strings, and these messages reach logs.
fn describe(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut message = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_credentials_and_contents() {
        let request = Request::new(Method::Post, "https://api.example/v1/x?api_key=sk_live_1")
            .header("X-Api-Key", "sk_live_2")
            .bearer_auth("sk_live_3")
            .json(serde_json::json!({"to": "Jane Doe, 1 Main St"}));
        let debug = format!("{request:?}");
        assert!(!debug.contains("sk_live"), "{debug}");
        assert!(!debug.contains("Jane"), "{debug}");
        assert!(debug.contains("X-Api-Key"), "{debug}");

        let basic = format!(
            "{:?}",
            Request::new(Method::Get, "u").basic_auth("sk_4", None)
        );
        assert!(!basic.contains("sk_4"), "{basic}");
    }

    #[test]
    fn replay_safety() {
        assert!(Request::new(Method::Get, "u").is_replay_safe());
        assert!(Request::new(Method::Delete, "u").is_replay_safe());
        assert!(!Request::new(Method::Post, "u").is_replay_safe());
        assert!(!Request::new(Method::Patch, "u").is_replay_safe());
        assert!(
            Request::new(Method::Post, "u")
                .idempotency_key("Idempotency-Key", "op_1")
                .is_replay_safe()
        );
    }

    #[test]
    fn retry_after_seconds() {
        let response = |v: &str| Response {
            status: 429,
            headers: vec![("retry-after".into(), v.into())],
            body: vec![],
        };
        assert_eq!(response("2").retry_after(), Some(Duration::from_secs(2)));
        assert_eq!(
            response("Wed, 21 Oct 2015 07:28:00 GMT").retry_after(),
            None
        );
    }
}
