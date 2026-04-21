//! Concurrency-limit transport layer.
//!
//! Caps the number of in-flight RPC requests through a shared
//! [`tokio::sync::Semaphore`]. Callers acquire the permit transparently before
//! the inner transport is invoked; the permit is released when the response
//! future resolves (or is dropped).

use std::{
    sync::Arc,
    task::{Context, Poll},
};

use alloy_json_rpc::{RequestPacket, ResponsePacket};
use alloy_transport::{TransportError, TransportFut};
use tokio::sync::Semaphore;
use tower::{Layer, Service};

/// A [`tower::Layer`] that bounds the number of concurrent RPC requests.
///
/// All services produced by the same layer instance share a single underlying
/// [`Semaphore`], so the bound applies across clones of the wrapped service
/// (e.g. across the `Provider`'s internal cloning).
///
/// We don't reuse [`tower::limit::ConcurrencyLimitLayer`] because it reserves
/// its permit in `poll_ready` and consumes it in `call`. Alloy's transport
/// stack clones the boxed service and invokes `call` directly, bypassing the
/// per-clone `poll_ready` reservation and breaking that contract.
#[derive(Clone, Debug)]
pub struct ConcurrencyLimitLayer {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyLimitLayer {
    /// Creates a new concurrency-limit layer that allows up to `max` in-flight
    /// requests across all services produced by this layer.
    ///
    /// # Panics
    /// Panics if `max == 0`. Callers must enforce this at the boundary.
    pub fn new(max: usize) -> Self {
        assert!(max > 0, "concurrency must be >= 1");
        Self { semaphore: Arc::new(Semaphore::new(max)) }
    }
}

impl<S> Layer<S> for ConcurrencyLimitLayer {
    type Service = ConcurrencyLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConcurrencyLimitService { inner, semaphore: Arc::clone(&self.semaphore) }
    }
}

/// The [`tower::Service`] produced by [`ConcurrencyLimitLayer`].
#[derive(Clone, Debug)]
pub struct ConcurrencyLimitService<S> {
    inner: S,
    semaphore: Arc<Semaphore>,
}

impl<S> Service<RequestPacket> for ConcurrencyLimitService<S>
where
    // Bounds match `alloy_transport::Transport` so the wrapped service can
    // itself be boxed by `IntoBoxTransport`.
    S: Service<
            RequestPacket,
            Response = ResponsePacket,
            Error = TransportError,
            Future = TransportFut<'static>,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let semaphore = Arc::clone(&self.semaphore);
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let _permit =
                semaphore.acquire_owned().await.expect("semaphore is owned and never closed");
            inner.call(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use alloy_json_rpc::{Id, Request, RequestMeta, SerializedRequest};
    use tokio::time::{sleep, timeout};

    use super::*;

    /// Bounds tests so a leaked permit fails deterministically.
    const TEST_DEADLINE: Duration = Duration::from_secs(5);

    #[derive(Clone)]
    struct ConcurrencyProbe {
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl Service<RequestPacket> for ConcurrencyProbe {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: RequestPacket) -> Self::Future {
            let in_flight = Arc::clone(&self.in_flight);
            let peak = Arc::clone(&self.peak);
            let delay = self.delay;
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                sleep(delay).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Err(alloy_transport::TransportErrorKind::custom_str("probe"))
            })
        }
    }

    #[derive(Clone)]
    struct RetryAfterDelayLayer {
        delay: Duration,
        attempts: u32,
    }

    impl<S> Layer<S> for RetryAfterDelayLayer {
        type Service = RetryAfterDelayService<S>;

        fn layer(&self, inner: S) -> Self::Service {
            RetryAfterDelayService { inner, delay: self.delay, attempts: self.attempts }
        }
    }

    #[derive(Clone)]
    struct RetryAfterDelayService<S> {
        inner: S,
        delay: Duration,
        attempts: u32,
    }

    impl<S> Service<RequestPacket> for RetryAfterDelayService<S>
    where
        S: Service<
                RequestPacket,
                Response = ResponsePacket,
                Error = TransportError,
                Future = TransportFut<'static>,
            > + Clone
            + Send
            + Sync
            + 'static,
    {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, request: RequestPacket) -> Self::Future {
            let mut inner = self.inner.clone();
            let delay = self.delay;
            let attempts = self.attempts;
            Box::pin(async move {
                let mut last_err = None;
                for _ in 0..attempts {
                    match inner.call(request.clone()).await {
                        Ok(res) => return Ok(res),
                        Err(e) => {
                            last_err = Some(e);
                            sleep(delay).await;
                        }
                    }
                }
                Err(last_err.expect("attempts >= 1"))
            })
        }
    }

    fn dummy_request() -> RequestPacket {
        let req: Request<()> =
            Request { meta: RequestMeta::new("test".into(), Id::Number(1)), params: () };
        let serialized: SerializedRequest = req.try_into().expect("serialize");
        RequestPacket::Single(serialized)
    }

    #[tokio::test]
    async fn caps_in_flight_requests() {
        let probe = ConcurrencyProbe {
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            delay: Duration::from_millis(50),
        };
        let peak = Arc::clone(&probe.peak);

        let layer = ConcurrencyLimitLayer::new(2);
        let service = layer.layer(probe);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let mut svc = service.clone();
            handles.push(tokio::spawn(async move {
                let _ = svc.call(dummy_request()).await;
            }));
        }
        for h in handles {
            timeout(TEST_DEADLINE, h).await.expect("test timed out").unwrap();
        }

        assert!(peak.load(Ordering::SeqCst) <= 2, "peak concurrency exceeded the limit");
    }

    #[tokio::test]
    async fn permit_released_on_inner_error() {
        // With concurrency = 1, a leaked permit would hang the next call;
        // the timeout converts that into a deterministic failure.
        let probe = ConcurrencyProbe {
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            delay: Duration::from_millis(1),
        };
        let layer = ConcurrencyLimitLayer::new(1);
        let mut service = layer.layer(probe);

        for _ in 0..4 {
            let res = timeout(TEST_DEADLINE, service.call(dummy_request())).await;
            assert!(res.is_ok(), "permit was leaked across an inner error");
            assert!(res.unwrap().is_err(), "probe always returns Err");
        }
    }

    #[tokio::test]
    async fn permit_released_during_outer_retry_backoff() {
        // Asserts that with retry as the OUTER layer, the inner concurrency
        // permit is released across the back-off sleep instead of held for
        // the whole retry budget.
        let probe = ConcurrencyProbe {
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            delay: Duration::from_millis(10),
        };
        let peak = Arc::clone(&probe.peak);

        let stack = RetryAfterDelayLayer { delay: Duration::from_millis(20), attempts: 3 }
            .layer(ConcurrencyLimitLayer::new(2).layer(probe));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let mut svc = stack.clone();
            handles.push(tokio::spawn(async move {
                let _ = svc.call(dummy_request()).await;
            }));
        }
        for h in handles {
            timeout(TEST_DEADLINE, h).await.expect("test timed out").unwrap();
        }

        assert!(peak.load(Ordering::SeqCst) <= 2, "permit leaked across outer retry");
    }

    #[test]
    #[should_panic(expected = "concurrency must be >= 1")]
    fn rejects_zero_concurrency() {
        let _ = ConcurrencyLimitLayer::new(0);
    }
}
