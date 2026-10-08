//! Cancellation and progress for a running synthesis.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use glam::IVec3;

/// A handle onto a running synthesis: cancel it, or read how far it got.
///
/// Cloning shares the same synthesis. Pass one clone to
/// [`synthesize_qasm_async`](crate::synthesize_qasm_async) and keep the other for
/// the UI thread; every method is safe to call while the search runs.
///
/// ```
/// # use bloq_lassynth::SynthesisMonitor;
/// let monitor = SynthesisMonitor::new();
/// assert!(!monitor.is_cancelled());
/// monitor.cancel();
/// assert!(monitor.is_cancelled());
/// ```
#[derive(Clone, Default)]
pub struct SynthesisMonitor(Arc<State>);

/// How far a synthesis has got, as of [`SynthesisMonitor::progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SynthesisProgress {
    /// SAT calls issued so far.
    pub solves: u32,
    /// Dimensions of the box handed to the running SAT call.
    pub size: IVec3,
}

impl SynthesisMonitor {
    /// Creates a monitor for one synthesis run.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the synthesis to stop, interrupting the SAT solver mid-search.
    ///
    /// The run then fails with [`SynthesisError::Cancelled`](crate::SynthesisError::Cancelled).
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
        self.0.interrupt_solver();
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }

    /// Reads the current progress counters.
    #[must_use]
    pub fn progress(&self) -> SynthesisProgress {
        let state = &self.0;
        SynthesisProgress {
            solves: state.solves.load(Ordering::Relaxed),
            size: IVec3::new(
                state.size[0].load(Ordering::Relaxed),
                state.size[1].load(Ordering::Relaxed),
                state.size[2].load(Ordering::Relaxed),
            ),
        }
    }

    pub(crate) fn begin_solve(&self, size: IVec3) {
        self.0.solves.fetch_add(1, Ordering::Relaxed);
        for (slot, value) in self.0.size.iter().zip(size.to_array()) {
            slot.store(value, Ordering::Relaxed);
        }
    }

    /// Publishes the native solver's interrupt handle.
    pub(crate) fn attach_solver(
        &self,
        interrupter: Box<dyn rustsat::solvers::InterruptSolver + Send>,
    ) {
        let mut slot = self
            .0
            .interrupter
            .lock()
            .expect("monitor mutex is poisoned");
        *slot = Some(interrupter);
        // Cover a cancel between the pre-solve check and this attach.
        if self.is_cancelled()
            && let Some(interrupter) = slot.as_ref()
        {
            interrupter.interrupt();
        }
    }

    /// Retracts the solver-borrowing interrupt handle.
    pub(crate) fn detach_solver(&self) {
        *self
            .0
            .interrupter
            .lock()
            .expect("monitor mutex is poisoned") = None;
    }

    pub(crate) fn interrupt_solver(&self) {
        self.0.interrupt_solver();
    }
}

impl fmt::Debug for SynthesisMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SynthesisMonitor")
            .field("cancelled", &self.is_cancelled())
            .field("progress", &self.progress())
            .finish()
    }
}

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    solves: AtomicU32,
    size: [AtomicI32; 3],
    /// Held only while its Kissat solver is alive.
    interrupter: std::sync::Mutex<Option<Box<dyn rustsat::solvers::InterruptSolver + Send>>>,
}

impl State {
    fn interrupt_solver(&self) {
        if let Some(interrupter) = self
            .interrupter
            .lock()
            .expect("monitor mutex is poisoned")
            .as_ref()
        {
            interrupter.interrupt();
        }
    }
}
