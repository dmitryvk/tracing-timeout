#![doc = include_str!("../README.md")]

use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use pin_project_lite::pin_project;
use tracing::{Level, span};

use crate::waker::{TracingTimeoutWaker, TracingTimeoutWakerInner};

pub use crate::{trace::CaptureTrace, trace::DefaultTraceCapturer, trace::StackAndSpanTrace};

#[cfg(test)]
mod tests;
mod trace;
mod waker;

pub fn timeout<C, Fut>(duration: Duration, capture: C, fut: Fut) -> TimeoutFuture<C, Fut> {
    let deadline = tokio::time::sleep(duration);
    TimeoutFuture {
        deadline,
        capture: Some(capture),
        inner: fut,
    }
}

pin_project! {
    pub struct TimeoutFuture<C, Fut> {
        #[pin]
        deadline: tokio::time::Sleep,
        capture: Option<C>,
        #[pin]
        inner: Fut,
    }
}

impl<C, Fut> Future for TimeoutFuture<C, Fut>
where
    C: CaptureTrace + Send + 'static,
    Fut: Future,
{
    type Output = Result<Fut::Output, TimeoutElapsed<C::Trace>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        // A span just so that nested timeouts had some
        let deadline_span = span!(Level::TRACE, "deadline");
        let guard = deadline_span.enter();
        match this.deadline.poll(cx) {
            Poll::Ready(()) => {
                drop(guard);

                // We hit the timeout. Do one final poll for the inner future, but collect the traces this time.
                // TODO: we don't have to call cx.waker.clone(), but it probably does not matter much since we already hit timeout
                let Some(capture) = this.capture.take() else {
                    return Poll::Ready(Err(TimeoutElapsed {
                        active_traces: Vec::new(),
                    }));
                };
                let waker_inner = TracingTimeoutWakerInner::new(capture, cx.waker().clone());
                let waker = TracingTimeoutWaker::new(waker_inner.clone()).as_std_waker();
                let mut cx2 = Context::from_waker(&waker);
                match this.inner.poll(&mut cx2) {
                    Poll::Pending => {}
                    Poll::Ready(result) => return Poll::Ready(Ok(result)),
                }
                let active_traces: Vec<_> = waker_inner.traces();
                return Poll::Ready(Err(TimeoutElapsed { active_traces }));
            }
            Poll::Pending => {}
        }
        drop(guard);
        match this.inner.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(Ok(result)),
        }
    }
}

#[derive(Debug)]
pub struct TimeoutElapsed<Trace> {
    pub active_traces: Vec<Trace>,
}
