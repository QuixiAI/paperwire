//! Retries that cannot duplicate a provider-side effect.
//!
//! | Outcome                              | Retried?                          |
//! |--------------------------------------|-----------------------------------|
//! | [`HttpError::NotSent`]               | yes, any method: nothing was sent |
//! | 429 / 502 / 503 / 504 response       | only if [`Request::is_replay_safe`] |
//! | any other response                   | no                                |
//! | [`HttpError::Ambiguous`]             | **never** (design.md §19.2)       |
//! | [`HttpError::ResponseTooLarge`]      | no: the provider already answered |
//! | [`HttpError::InvalidRequest`]        | no                                |

use std::time::Duration;

use super::{HttpError, Request, Response, Transport};

/// Statuses that usually mean "try again later". A gateway error on a
/// non-replay-safe request is not retried: the upstream may have acted.
const RETRY_STATUSES: [u16; 4] = [429, 502, 503, 504];

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts, including the first. 1 disables retries.
    pub max_attempts: u32,
    /// Delay before the first retry; doubles on each subsequent one.
    pub base_delay: Duration,
    /// Cap on any single delay. A `Retry-After` longer than this is not
    /// waited out: the response is returned instead.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff for the delay after attempt `attempt` (1-based).
    fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }

    /// How long to wait before retrying, or `None` to return `result`.
    fn delay_after(
        &self,
        request: &Request,
        result: &Result<Response, HttpError>,
        attempt: u32,
    ) -> Option<Duration> {
        if attempt >= self.max_attempts {
            return None;
        }
        match result {
            Err(HttpError::NotSent(_)) => Some(jitter(self.backoff(attempt))),
            Ok(response)
                if request.is_replay_safe() && RETRY_STATUSES.contains(&response.status) =>
            {
                match response.retry_after() {
                    Some(wait) if wait > self.max_delay => None,
                    Some(wait) => Some(wait),
                    None => Some(jitter(self.backoff(attempt))),
                }
            }
            Ok(_)
            | Err(
                HttpError::Ambiguous(_)
                | HttpError::InvalidRequest(_)
                | HttpError::ResponseTooLarge { .. },
            ) => None,
        }
    }
}

/// A random delay in `[delay / 2, delay]`, so clients that failed together
/// do not retry together. A provider's `Retry-After` is not jittered.
fn jitter(delay: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    // Each `RandomState` is randomly seeded: randomness without a dependency.
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let half = delay / 2;
    let span = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(random % span.saturating_add(1))
}

/// A [`Transport`] that retries the inner transport per [`RetryPolicy`].
#[derive(Debug, Clone)]
pub struct Retrying<T> {
    inner: T,
    policy: RetryPolicy,
}

impl<T> Retrying<T> {
    pub fn new(inner: T, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }
}

impl<T: Transport> Transport for Retrying<T> {
    async fn send(&self, request: Request) -> Result<Response, HttpError> {
        let mut attempt = 1;
        loop {
            let result = self.inner.send(request.clone()).await;
            let Some(delay) = self.policy.delay_after(&request, &result, attempt) else {
                return result;
            };
            tracing::warn!(
                method = ?request.method,
                attempt,
                outcome = %match &result {
                    Ok(r) => r.status.to_string(),
                    Err(e) => e.to_string(),
                },
                delay_ms = delay.as_millis() as u64,
                "retrying provider request"
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::super::Method;
    use super::*;

    /// Replays scripted outcomes and counts calls.
    struct Scripted {
        outcomes: Mutex<VecDeque<Result<Response, HttpError>>>,
        calls: Mutex<u32>,
    }

    impl Scripted {
        fn new(outcomes: Vec<Result<Response, HttpError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    impl Transport for &Scripted {
        async fn send(&self, _: Request) -> Result<Response, HttpError> {
            *self.calls.lock().unwrap() += 1;
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("more calls than scripted outcomes")
        }
    }

    fn status(code: u16) -> Result<Response, HttpError> {
        Ok(Response {
            status: code,
            headers: vec![],
            body: vec![],
        })
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(50),
        }
    }

    fn post() -> Request {
        Request::new(Method::Post, "http://provider.test/")
    }

    async fn run(script: &Scripted, request: Request) -> Result<Response, HttpError> {
        Retrying::new(script, fast()).send(request).await
    }

    #[tokio::test]
    async fn not_sent_is_retried_even_for_post() {
        let script = Scripted::new(vec![Err(HttpError::NotSent("refused".into())), status(201)]);
        assert_eq!(run(&script, post()).await.unwrap().status, 201);
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test]
    async fn ambiguous_is_never_retried() {
        let script = Scripted::new(vec![Err(HttpError::Ambiguous("timeout".into()))]);
        let err = run(&script, post().idempotency_key("Idempotency-Key", "op_1"))
            .await
            .unwrap_err();
        assert!(matches!(err, HttpError::Ambiguous(_)));
        assert_eq!(script.calls(), 1);
    }

    #[tokio::test]
    async fn unavailable_post_without_key_is_not_retried() {
        let script = Scripted::new(vec![status(503)]);
        assert_eq!(run(&script, post()).await.unwrap().status, 503);
        assert_eq!(script.calls(), 1);
    }

    #[tokio::test]
    async fn unavailable_post_with_key_is_retried() {
        let script = Scripted::new(vec![status(503), status(502), status(200)]);
        let response = run(&script, post().idempotency_key("Idempotency-Key", "op_1"))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(script.calls(), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_with_last_outcome() {
        let script = Scripted::new(vec![status(503), status(503), status(504)]);
        let get = Request::new(Method::Get, "http://provider.test/");
        assert_eq!(run(&script, get).await.unwrap().status, 504);
        assert_eq!(script.calls(), 3);
    }

    #[tokio::test]
    async fn client_errors_are_not_retried() {
        for code in [400, 401, 404, 409, 422, 500] {
            let script = Scripted::new(vec![status(code)]);
            let get = Request::new(Method::Get, "http://provider.test/");
            assert_eq!(run(&script, get).await.unwrap().status, code);
            assert_eq!(script.calls(), 1, "{code}");
        }
    }

    #[tokio::test]
    async fn retry_after_beyond_max_delay_is_returned_not_waited() {
        let script = Scripted::new(vec![Ok(Response {
            status: 429,
            headers: vec![("retry-after".into(), "120".into())],
            body: vec![],
        })]);
        let get = Request::new(Method::Get, "http://provider.test/");
        assert_eq!(run(&script, get).await.unwrap().status, 429);
        assert_eq!(script.calls(), 1);
    }

    #[tokio::test]
    async fn too_large_response_is_not_retried() {
        let script = Scripted::new(vec![Err(HttpError::ResponseTooLarge {
            head: status(503).unwrap(),
            limit: 1,
        })]);
        let get = Request::new(Method::Get, "http://provider.test/");
        assert!(run(&script, get).await.is_err());
        assert_eq!(script.calls(), 1);
    }

    #[test]
    fn jitter_stays_within_half_to_full_delay() {
        let delay = Duration::from_millis(400);
        for _ in 0..200 {
            let j = jitter(delay);
            assert!(j >= delay / 2 && j <= delay, "{j:?}");
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        let delays: Vec<_> = (1..=5).map(|a| policy.backoff(a).as_millis()).collect();
        assert_eq!(delays, [100, 200, 400, 500, 500]);
    }
}
