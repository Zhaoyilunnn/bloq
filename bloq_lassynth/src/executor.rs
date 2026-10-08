//! Runs the native synthesis future on the calling thread.
//!
//! Native synthesis has no suspension points. The synchronous entry points
//! drive the existing async API without an executor dependency.

use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Runs `future` to completion on the current thread.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::hint::spin_loop();
    }
}
